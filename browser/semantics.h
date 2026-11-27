// Private, per-tab CDP client. There is no debugging port or caller-supplied
// protocol method. Every continuation rechecks the request's control lease.
class Semantics final : public CefDevToolsMessageObserver {
  using Ticket = uint64_t;
  using Clock = std::chrono::steady_clock;
  struct Target {
    int backend;
    std::string role, name;
  };
  struct Request {
    Json args;
    uint64_t document;
    bool dispatched = false, poll = false, objects = false;
    std::string promise;
    std::shared_ptr<Evaluation> evaluation;
    Clock::time_point deadline = Clock::time_point::max();
  };
  struct Call {
    Ticket ticket;
    std::function<void(const Json &)> next;
  };
  B browser;
  CefRefPtr<CefRegistration> observer;
  std::function<bool(uint32_t)> permitted;
  std::function<std::string()> address;
  std::map<Ticket, Request> requests;
  std::map<int, Call> calls;
  std::map<std::string, Target> refs;
  bool ticking = false, attached = false;
  int serial = 0;
  uint64_t document = 0;
  static constexpr const char *stale = "Stale element reference; inspect again";

  void call(Ticket t, const char *method, Json params,
            std::function<void(const Json &)> next = {}) {
    // Count submitted calls until Chromium replies, even after cancellation.
    if (calls.size() >= 16)
      throw std::runtime_error(
          "Browser inspection busy; retry after pending calls finish");
    int id = ++serial;
    auto wire =
        Json{{"id", id}, {"method", method}, {"params", std::move(params)}}
            .dump();
    if (!live(t))
      return;
    calls.emplace(id, Call{t, std::move(next)});
    if (!browser->GetHost()->SendDevToolsMessage(wire.data(), wire.size())) {
      calls.erase(id);
      throw std::runtime_error("Browser inspection channel is unavailable");
    }
  }
  void finish(Ticket t, Json result) {
    auto it = requests.find(t);
    if (it == requests.end())
      return;
    bool objects = it->second.objects;
    requests.erase(it);
    if (objects && attached) {
      // Release transient DOM handles even if a later step was cancelled.
      auto wire = Json{{"id", ++serial},
                       {"method", "Runtime.releaseObjectGroup"},
                       {"params", {{"objectGroup", std::to_string(t)}}}}
                      .dump();
      browser->GetHost()->SendDevToolsMessage(wire.data(), wire.size());
    }
    result["req"] = t;
    result["url"] = address();
    emit(result);
  }
  bool live(Ticket t) {
    auto it = requests.find(t);
    if (it == requests.end())
      return false;
    if (!permitted(it->second.args.at("lease"))) {
      finish(t, {{"error", "User has browser control"}});
      return false;
    }
    if (Clock::now() >= it->second.deadline) {
      const auto &j = it->second.args;
      if (j.at("op") == "evaluate")
        finish(t, {{"error", "Page evaluation timed out; dispatched effects "
                             "were not undone"}});
      else {
        auto condition = j.contains("url")
                             ? j.at("url").dump()
                             : j.at("role").dump() + " " + j.at("name").dump() +
                                   " " + j.at("state").dump();
        finish(t, {{"error", "Timed out waiting for " + condition}});
      }
      return false;
    }
    if (it->second.document != document && it->second.args.at("op") != "wait" &&
        !it->second.dispatched) {
      finish(t, {{"error", stale}});
      return false;
    }
    return true;
  }
  // One timer per tab, not a timeout task per (possibly already cancelled)
  // wait.
  void schedule() {
    if (ticking)
      return;
    ticking = true;
    CefPostDelayedTask(
        TID_UI, new Work([self = CefRefPtr<Semantics>(this)] { self->tick(); }),
        100);
  }
  void tick() {
    ticking = false;
    std::vector<Ticket> waits;
    for (auto &[t, r] : requests)
      if (r.deadline != Clock::time_point::max())
        waits.push_back(t);
    for (auto t : waits) {
      if (!live(t))
        continue;
      auto &r = requests.at(t);
      if (r.poll) {
        try {
          if (r.args.at("op") == "evaluate")
            read_evaluation(t);
          else
            sample(t);
        } catch (const std::exception &e) {
          finish(t, {{"error", e.what()}});
        }
      }
    }
    if (!waits.empty())
      schedule();
  }

  void read_evaluation(Ticket t) {
    auto &r = requests.at(t);
    r.poll = false;
    call(t, "Runtime.getProperties",
         {{"objectId", r.promise},
          {"ownProperties", true},
          {"nonIndexedPropertiesOnly", true}});
  }
  void evaluate(Ticket t) {
    auto &r = requests.at(t);
    r.evaluation =
        std::make_shared<Evaluation>(r.args.at("file").get<std::string>());
    r.objects = true;
    r.dispatched = true;
    // CDP defaults to the inspected main frame's page world. No console API,
    // user activation, CSP bypass, caller-selected CDP method, or debugger
    // port.
    call(t, "Runtime.evaluate",
         {{"expression",
           "(\n" + r.args.at("expression").get<std::string>() + "\n)"},
          {"objectGroup", std::to_string(t)},
          {"awaitPromise", false},
          {"allowUnsafeEvalBlockedByCSP", false},
          {"timeout", r.args.at("timeout_ms")}});
  }
  void evaluated(Ticket t, Evaluation::Step reply) {
    if (!live(t))
      return;
    switch (reply.next) {
    case Evaluation::Step::Done:
      finish(t, std::move(reply.data));
      break;
    case Evaluation::Step::Promise:
      if (reply.data.is_null())
        requests.at(t).poll = true;
      else {
        requests.at(t).promise = reply.data.get<std::string>();
        read_evaluation(t);
      }
      break;
    case Evaluation::Step::Properties:
      call(t, "Runtime.getProperties", std::move(reply.data));
      break;
    case Evaluation::Step::Compare:
      call(t, "Runtime.callFunctionOn", std::move(reply.data));
      break;
    }
  }
  void evaluation_response(Ticket t, int id, bool success, const char *p,
                           size_t size) {
    // CEF owns p only during the callback. Copy once; keep this call counted
    // while decoding and spooling, even if cancelled. Only one step per
    // request is in flight, so the decoder is never accessed concurrently.
    calls.emplace(id, Call{t, {}});
    auto decoder = requests.at(t).evaluation;
    auto work = [self = CefRefPtr<Semantics>(this), t, id, success,
                 bytes = std::string(p, size), decoder] {
      auto reply = decoder->receive(success, bytes);
      CefPostTask(TID_UI,
                  new Work([self, t, id, reply = std::move(reply)]() mutable {
                    self->calls.erase(id);
                    try {
                      self->evaluated(t, std::move(reply));
                    } catch (const std::exception &e) {
                      self->finish(t, {{"error", e.what()}});
                    }
                  }));
    };
    if (!CefPostTask(TID_FILE_USER_BLOCKING, new Work(std::move(work)))) {
      calls.erase(id);
      evaluated(t, decoder->receive(success, std::string(p, size)));
    }
  }
  void sample(Ticket t) {
    requests.at(t).poll = false;
    const auto &j = requests.at(t).args;
    if (j.contains("url")) {
      if (address() == j.at("url").get_ref<const std::string &>())
        finish(t, Json::object());
      else
        requests.at(t).poll = true;
      return;
    }
    call(
        t, "Accessibility.getFullAXTree", Json::object(),
        [this, t, doc = document](const Json &tree) {
          const auto &j = requests.at(t).args;
          bool inspect = j.at("op") == "inspect";
          if (doc != document) {
            if (inspect)
              finish(t, {{"error", stale}});
            else
              requests.at(t).poll = true;
            return;
          }
          // getFullAXTree defaults to the main frame. Walk child IDs (not wire
          // order) to preserve hierarchy and exclude editable values/iframe
          // trees.
          const auto &nodes = tree.at("nodes");
          std::map<std::string, const Json *> index;
          std::vector<std::pair<const Json *, int>> stack;
          for (const auto &node : nodes) {
            index.emplace(node.at("nodeId"), &node);
            if (!node.contains("parentId"))
              stack.emplace_back(&node, 0);
          }
          // Chromium can compute a control's name from another field's value.
          // Mark label containers that enclose fields. A normal wrapping label
          // for the field itself remains usable; an embedded *other* value is
          // not.
          std::map<int, int> fields;
          std::set<int> known;
          for (const auto &n : nodes) {
            int backend = n.value("backendDOMNodeId", 0);
            known.insert(backend);
            if (!backend || !n.contains("value"))
              continue;
            const Json *ancestor = &n;
            while (ancestor) {
              int id = ancestor->value("backendDOMNodeId", 0);
              if (id) {
                auto [i, inserted] = fields.emplace(id, backend);
                if (!inserted) {
                  if (!i->second || i->second == backend)
                    break;
                  i->second = 0;
                }
              }
              auto parent =
                  index.find(ancestor->value("parentId", std::string()));
              ancestor = parent == index.end() ? nullptr : parent->second;
            }
          }
          Json result = Json::array();
          std::map<std::string, Target> fresh;
          bool matched = false;
          while (!stack.empty()) {
            auto [ptr, depth] = stack.back();
            stack.pop_back();
            const auto &n = *ptr;
            std::string role = n.contains("role")
                                   ? n.at("role").value("value", std::string())
                                   : "";
            std::string name = n.contains("name")
                                   ? n.at("name").value("value", std::string())
                                   : "";
            const std::string ax_name = name, ax_role = role;
            int backend = n.value("backendDOMNodeId", 0);
            auto may_embed_value = [&](int source) {
              auto field = fields.find(source);
              return !known.contains(source) ||
                     (field != fields.end() && field->second != backend);
            };
            if (n.contains("name") && n.at("name").contains("sources"))
              for (const auto &src : n.at("name").at("sources")) {
                if (src.value("superseded", false))
                  continue;
                if (src.value("type", std::string()) == "contents" &&
                    may_embed_value(backend))
                  name.clear();
                for (const auto *k :
                     {"value", "attributeValue", "nativeSourceValue"})
                  if (src.contains(k) && src.at(k).contains("relatedNodes"))
                    for (const auto &related : src.at(k).at("relatedNodes"))
                      if (may_embed_value(related.value("backendDOMNodeId", 0)))
                        name.clear();
              }
            bool shown = !n.at("ignored").get<bool>() &&
                         role != "InlineTextBox" && role != "none" &&
                         !(role == "generic" && name.empty());
            Json node;
            bool editable = role == "textbox" || role == "searchbox" ||
                            role == "combobox" || role == "spinbutton";
            if (role == "StaticText")
              role = "text";
            if (role == "RootWebArea")
              role = "document";
            if (n.contains("properties"))
              for (const auto &p : n.at("properties")) {
                auto key = p.at("name").get<std::string>();
                if (key == "editable")
                  editable = true;
                if (key == "disabled" || key == "checked" ||
                    key == "required" || key == "expanded" ||
                    key == "selected" || key == "readonly" || key == "level" ||
                    key == "pressed")
                  node[key] = p.at("value").at("value");
              }
            if (shown && inspect) {
              node["depth"] = depth;
              node["role"] = role;
              node["name"] = name;
              if (role != "text" && role != "document" &&
                  n.contains("backendDOMNodeId")) {
                auto ref = j.at("snapshot").get<std::string>() + ":" +
                           n.at("nodeId").get<std::string>();
                node["ref"] = ref;
                fresh.emplace(ref, Target{backend, ax_role, ax_name});
              }
              result.push_back(std::move(node));
            } else if (shown &&
                       role == j.at("role").get_ref<const std::string &>() &&
                       name == j.at("name").get_ref<const std::string &>() &&
                       (j.at("state") != "enabled" || node.is_null() ||
                        !node.value("disabled", false))) {
              matched = true;
              break;
            }
            if (!editable && role != "Iframe" &&
                role != "IframePresentational" && n.contains("childIds")) {
              const auto &children = n.at("childIds");
              for (auto i = children.rbegin(); i != children.rend(); ++i)
                if (auto found = index.find(i->get<std::string>());
                    found != index.end())
                  stack.emplace_back(found->second, depth + int(shown));
            }
          }
          if (inspect) {
            refs = std::move(fresh);
            finish(t, {{"nodes", std::move(result)}});
          } else if (matched != (j.at("state") == "disappeared")) {
            finish(t, Json::object());
          } else {
            requests.at(t).poll = true;
          }
        });
  }
  void element(Ticket t) {
    const auto &j = requests.at(t).args;
    auto it = refs.find(j.at("ref").get<std::string>());
    if (it == refs.end())
      throw std::runtime_error(stale);
    Target target = it->second;
    int backend = target.backend;
    call(t, "Accessibility.getPartialAXTree",
         {{"backendNodeId", backend}, {"fetchRelatives", false}},
         [this, t, target](const Json &tree) {
           for (const auto &node : tree.at("nodes")) {
             if (!node.at("ignored").get<bool>() &&
                 node.value("backendDOMNodeId", 0) == target.backend &&
                 node.at("role").at("value").get<std::string>() ==
                     target.role &&
                 (node.contains("name")
                      ? node.at("name").value("value", std::string())
                      : "") == target.name) {
               resolve(t, target.backend);
               return;
             }
           }
           throw std::runtime_error(stale);
         });
  }
  void resolve(Ticket t, int backend) {
    call(
        t, "Page.getFrameTree", Json::object(),
        [this, t, backend](const Json &page) {
          call(
              t, "Page.createIsolatedWorld",
              {{"frameId", page.at("frameTree").at("frame").at("id")},
               {"worldName", "Orbit browser controls"}},
              [this, t, backend](const Json &world) {
                requests.at(t).objects = true;
                call(
                    t, "DOM.resolveNode",
                    {{"backendNodeId", backend},
                     {"executionContextId", world.at("executionContextId")},
                     {"objectGroup", std::to_string(t)}},
                    [this, t](const Json &resolved) {
                      auto &r = requests.at(t);
                      bool fill = r.args.at("op") == "fill";
                      // Both checks and effects run synchronously on the
                      // resolved node, never on a CSS selector, screen
                      // coordinate, or subsequently focused field.
                      static constexpr auto act = R"JS(function(fill,text) {
            if (!(this instanceof HTMLElement) || !this.isConnected || this.ownerDocument !== document)
              return "Stale element reference; inspect again";
            if (!this.checkVisibility({visibilityProperty:true}) || !this.getClientRects().length || this.matches(':disabled') || this.closest('[inert],[aria-disabled="true"]'))
              return "Element is hidden or disabled";
            if (!fill) { this.click(); return ""; }
            if (this.readOnly || this.getAttribute('aria-readonly') === 'true')
              return "Field is read-only";
            if (this instanceof HTMLTextAreaElement || (this instanceof HTMLInputElement && ['text','email','tel','url','password','search'].includes(this.type))) {
              this.value = text;
            } else if (this.isContentEditable) {
              this.textContent = text;
            } else return "Element is not an editable text field";
            this.dispatchEvent(new InputEvent('input',{bubbles:true,composed:true,inputType:'insertReplacementText',data:text}));
            this.dispatchEvent(new Event('change',{bubbles:true}));
            return "";
          })JS";
                      r.dispatched = true;
                      call(
                          t, "Runtime.callFunctionOn",
                          {{"objectId", resolved.at("object").at("objectId")},
                           {"functionDeclaration", act},
                           {"arguments",
                            Json::array(
                                {Json{{"value", fill}},
                                 Json{{"value",
                                       r.args.value("text", std::string())}}})},
                           {"returnByValue", true},
                           {"userGesture", true}},
                          [this, t](const Json &v) {
                            if (v.contains("exceptionDetails"))
                              throw std::runtime_error("Element action failed");
                            auto error =
                                v.at("result").at("value").get<std::string>();
                            finish(t, error.empty() ? Json::object()
                                                    : Json{{"error", error}});
                          });
                    });
              });
        });
  }

public:
  Semantics(B b, std::function<bool(uint32_t)> allowed,
            std::function<std::string()> url)
      : browser(b), permitted(std::move(allowed)), address(std::move(url)) {}
  void start(Json j) {
    if (!observer)
      observer = browser->GetHost()->AddDevToolsMessageObserver(this);
    Ticket t = j.at("req");
    auto op = j.at("op");
    if (requests.size() >= 16) {
      emit({{"req", t}, {"error", "Too many pending browser requests"}});
      return;
    }
    requests.emplace(t, Request{std::move(j), document});
    try {
      if (op == "wait" || op == "evaluate") {
        auto &r = requests.at(t);
        r.deadline = Clock::now() + std::chrono::milliseconds(
                                        r.args.at("timeout_ms").get<int>());
        schedule();
      }
      if (op == "evaluate")
        evaluate(t);
      else if (op == "inspect" || op == "wait")
        sample(t);
      else
        element(t);
    } catch (const std::exception &e) {
      finish(t, {{"error", e.what()}});
    }
  }
  void OnDevToolsMethodResult(B, int id, bool success, const void *data,
                              size_t size) override {
    auto it = calls.find(id);
    if (it == calls.end())
      return;
    auto [t, next] = std::move(it->second);
    calls.erase(it);
    if (!live(t))
      return;
    try {
      auto p = static_cast<const char *>(data);
      if (!next) {
        evaluation_response(t, id, success, p, size);
        return;
      }
      if (!success && requests.at(t).args.at("op") == "wait") {
        requests.at(t).poll = true;
        return;
      }
      if (!success)
        throw std::runtime_error(requests.at(t).args.at("op") == "fill" ||
                                         requests.at(t).args.at("op") ==
                                             "activate"
                                     ? stale
                                     : "Accessibility tree unavailable");
      next(Json::parse(p, p + size));
    } catch (const std::exception &e) {
      finish(t, {{"error", e.what()}});
    }
  }
  void navigation() {
    ++document;
    refs.clear();
  }
  void new_document() {
    navigation();
    std::vector<Ticket> pending;
    for (auto &[t, r] : requests)
      if (r.args.at("op") == "evaluate")
        pending.push_back(t);
    for (auto t : pending)
      finish(t, {{"error", "Page navigated before evaluation completed; "
                           "dispatched effects were not undone"}});
  }
  void cancel(Ticket t) { finish(t, {{"error", "Browser request cancelled"}}); }
  void clear(const char *why) {
    navigation();
    while (!requests.empty())
      finish(requests.begin()->first, {{"error", why}});
  }
  void close() {
    clear("Tab closed");
    observer = nullptr;
    browser = nullptr;
  }
  void OnDevToolsAgentAttached(B) override { attached = true; }
  void OnDevToolsAgentDetached(B) override {
    attached = false;
    calls.clear();
    clear("Browser renderer disconnected");
  }
  IMPLEMENT_REFCOUNTING(Semantics);
};
