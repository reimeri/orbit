// Decode only own data descriptors. CDP deep serialization and ordinary
// JSON.stringify can invoke page getters after the evaluated script returns.
// All traversal and spooling runs on the file thread; only the UI thread
// submits the next CDP step, after rechecking the request deadline and lease.
class Evaluation {
public:
  struct Step {
    enum Next { Done, Promise, Properties, Compare } next;
    Json data;
  };
  static constexpr size_t budget = 64 * 1024 * 1024;
  explicit Evaluation(const std::string &path) {
    // Rust owns the temporary file. Never recreate it after cancellation.
    int fd = open(path.c_str(), O_WRONLY | O_CLOEXEC | O_NOFOLLOW | O_NONBLOCK);
    struct stat st{};
    if (fd >= 0 && fstat(fd, &st) == 0 && S_ISREG(st.st_mode) &&
        st.st_uid == geteuid() && !(st.st_mode & 0077))
      file.reset(fdopen(fd, "wb"));
    if (!file) {
      if (fd >= 0)
        ::close(fd);
      throw std::runtime_error(
          "Could not open private result file; expression was not run");
    }
  }
  Step receive(bool success, const std::string &bytes) {
    try {
      // Bound work before parsing descriptors or expanding shared subtrees.
      if (bytes.size() > budget - received || ++steps > 16384)
        return error("Result exceeds the evaluation traversal budget; return a smaller projection");
      received += bytes.size();
      Json reply = Json::parse(bytes);
      if (!success)
        return error(reply.at("message").get<std::string>());
      if (reply.contains("exceptionDetails")) {
        const auto &d = reply.at("exceptionDetails");
        return error(d.contains("exception") ? reason(d.at("exception"))
                                             : d.at("text").get<std::string>());
      }
      switch (phase) {
      case Initial:
        return visit(std::move(reply.at("result")));
      case PromiseState: {
        const Json *state = nullptr, *result = nullptr;
        if (!reply.contains("internalProperties"))
          return error("Chromium did not provide the promise state");
        for (const auto &p : reply.at("internalProperties")) {
          if (!p.contains("value")) continue;
          if (p.at("name") == "[[PromiseState]]" && p.at("value").contains("value"))
            state = &p.at("value").at("value");
          if (p.at("name") == "[[PromiseResult]]")
            result = &p.at("value");
        }
        if (state && *state == "pending")
          return {Step::Promise, nullptr};
        if (!result || !state)
          return error("Chromium did not provide the promise result");
        if (*state == "rejected")
          return error(reason(*result));
        return visit(*result);
      }
      case Identity:
        if (reply.at("result").at("value").get<bool>())
          return error("Not JSON data: cyclic object");
        return properties();
      case Descriptors:
        descriptors += reply.at("result").size();
        if (descriptors > 262144)
          return error("Result exceeds evaluation property budget; return a smaller projection");
        frames.back().props = std::move(reply.at("result"));
        return advance();
      }
    } catch (const std::exception &e) {
      return error(e.what());
    }
    std::terminate();
  }

private:
  enum Phase { Initial, PromiseState, Identity, Descriptors } phase = Initial;
  struct Frame {
    std::string id;
    Json value, props;
    size_t index = 0;
    std::string key;
    size_t length = 0;
  };
  size_t received = 0, steps = 0, descriptors = 0;
  std::vector<Frame> frames;
  std::unique_ptr<FILE, decltype(&fclose)> file{nullptr, &fclose};

  Step output(std::string raw, bool failed) {
    if (raw.size() > 40000 &&
        fwrite(raw.data(), 1, raw.size(), file.get()) == raw.size() &&
        fflush(file.get()) == 0)
      return {
          Step::Done,
          {{"eval_spill", failed ? "error" : "value"}, {"bytes", raw.size()}}};
    // On a write failure the complete payload is still available to Rust.
    return {Step::Done,
            {{failed ? "error" : "evaluation_json", std::move(raw)}}};
  }
  Step error(std::string why) {
    return output("Page evaluation: " + why, true);
  }
  static std::string reason(const Json &e) {
    if (e.contains("value")) {
      const auto &v = e.at("value");
      return v.is_string() ? v.get<std::string>() : v.dump();
    }
    return e.value("description",
                   e.value("type", std::string("unknown exception")));
  }
  static bool scalar(const Json &r) {
    auto type = r.at("type");
    return !r.contains("unserializableValue") && r.contains("value") &&
           (type == "string" || type == "boolean" || type == "number" ||
            r.at("value").is_null());
  }
  Step visit(Json r) {
    if (scalar(r))
      return output(r.at("value").dump(), false);
    if (phase == Initial && r.value("subtype", std::string()) == "promise") {
      phase = PromiseState;
      return {Step::Promise, r.at("objectId")};
    }
    return descend(r);
  }
  Step properties() {
    phase = Descriptors;
    return {Step::Properties,
            {{"objectId", frames.back().id}, {"ownProperties", true}}};
  }
  Step descend(const Json &r) {
    if (frames.size() >= 128)
      return error("Result exceeds evaluation depth limit; return a shallower projection");
    bool array = r.value("subtype", std::string()) == "array";
    if (r.at("type") != "object" ||
        (!array && (r.contains("subtype") ||
                    r.value("className", std::string()) != "Object")))
      return error("Not JSON data: " +
                   r.value("subtype", r.at("type").get<std::string>()) +
                   "; return plain data properties instead");
    // Object IDs are handles, not identity: Chromium can give the same object
    // different IDs. Strict equality with ancestors does not run user code.
    Json ancestors = Json::array();
    for (const auto &f : frames)
      ancestors.push_back({{"objectId", f.id}});
    frames.push_back(Frame{r.at("objectId"),
                           array ? Json::array() : Json::object(),
                           Json::array()});
    if (ancestors.empty())
      return properties();
    phase = Identity;
    return {Step::Compare,
            {{"objectId", frames.back().id},
             {"arguments", std::move(ancestors)},
             {"functionDeclaration", "function(){'use strict';for(let "
                                     "i=0;i<arguments.length;++i)if(this==="
                                     "arguments[i])return true;return false}"},
             {"throwOnSideEffect", true},
             {"returnByValue", true}}};
  }
  static void append(Frame &f, Json value) {
    if (f.value.is_array())
      f.value.push_back(std::move(value));
    else
      f.value[f.key] = std::move(value);
  }
  Step advance() {
    for (;;) {
      auto &f = frames.back();
      while (f.index < f.props.size()) {
        const auto &p = f.props.at(f.index++);
        f.key = p.at("name").get<std::string>();
        if (f.value.is_array() && f.key == "length") {
          f.length = p.at("value").at("value").get<size_t>();
          continue;
        }
        if (p.contains("symbol") || !p.at("enumerable").get<bool>() ||
            !p.contains("value") || p.value("wasThrown", false))
          return error(
              "Not JSON data: accessor, symbol or non-enumerable property");
        if (f.value.is_array() && f.key != std::to_string(f.value.size()))
          return error("Not JSON data: sparse array or extra array property");
        const auto &v = p.at("value");
        if (!scalar(v))
          return descend(v);
        append(f, v.at("value"));
      }
      if (f.value.is_array() && f.value.size() != f.length)
        return error("Not JSON data: sparse array");
      Json value = std::move(f.value);
      frames.pop_back();
      if (frames.empty())
        return output(value.dump(), false);
      append(frames.back(), std::move(value));
    }
  }
};
