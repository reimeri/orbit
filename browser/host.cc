// CEF and all browser objects live on its UI thread. Only Orbit's private
// stdio pipes carry commands/events; there is no remote debugging endpoint.
#include "include/cef_app.h"
#include "include/cef_client.h"
#include "include/cef_cookie.h"
#include "include/cef_devtools_message_observer.h"
#include <atomic>
#include <chrono>
#include <cerrno>
#include <cmath>
#include <cstdio>
#include <cstring>
#include <fcntl.h>
#include <functional>
#include <iostream>
#include <map>
#include <memory>
#include <nlohmann/json.hpp>
#include <png.h>
#include <set>
#include <sys/mman.h>
#include <sys/stat.h>
#include <thread>
#include <unistd.h>

using Json = nlohmann::json;
using B = CefRefPtr<CefBrowser>;
using F = CefRefPtr<CefFrame>;
constexpr size_t limit = 4096, capacity = 64 + limit * limit * 4;
struct Header {
  uint32_t seq, width, height, lease, view_width, view_height;
  float scale;
};
static FILE *events;
static void emit(const Json &j) {
  std::fprintf(events, "%s\n", j.dump(-1, ' ', false, Json::error_handler_t::replace).c_str());
  std::fflush(events);
}
class Work : public CefTask {
  std::function<void()> work;

public:
  explicit Work(std::function<void()> f) : work(std::move(f)) {}
  void Execute() override { work(); }
  IMPLEMENT_REFCOUNTING(Work);
};
#include "evaluation.h"
#include "semantics.h"

class Flushed final : public CefCompletionCallback {
  B browser;

public:
  explicit Flushed(B b) : browser(b) {}
  void OnComplete() override { browser->GetHost()->CloseBrowser(true); }
  IMPLEMENT_REFCOUNTING(Flushed);
};
class Tab;
static std::map<uint64_t, CefRefPtr<Tab>> tabs;
static bool quitting;
// TID_UI owns admission; at most two detached paint buffers process-wide.
static int encoders;
static void command(Json);
static bool web(const std::string &u) {
  return u.starts_with("http://") || u.starts_with("https://") ||
         u == "about:blank";
}
class Tab final : public CefClient,
                  public CefRenderHandler,
                  public CefLifeSpanHandler,
                  public CefLoadHandler,
                  public CefDisplayHandler,
                  public CefRequestHandler,
                  public CefDialogHandler,
                  public CefContextMenuHandler {
public:
  uint64_t id;
  B browser;
  CefRefPtr<Semantics> semantics;
  std::vector<Json> deferred;
  void OnAfterCreated(B b) override {
    browser = b;
    if (quitting) {
      b->GetHost()->CloseBrowser(true);
      return;
    }
    refresh_visibility();
    for (auto &j : std::exchange(deferred, {}))
      command(std::move(j));
  }
  Header *mem;
  int w = 960, h = 720;
  float scale = 1;
  bool fixed = false;
  int panel_w = w, panel_h = h;
  float panel_scale = scale;
  bool base_ready = false;
  void size(int width, int height, float density) {
    if (width == w && height == h && density == scale)
      return;
    base_ready = false;
    w = width;
    h = height;
    scale = density;
    auto host = browser->GetHost();
    host->NotifyScreenInfoChanged();
    host->WasResized();
    host->Invalidate(PET_VIEW);
  }
  bool visible = false, allowed = true, closing = false;
  std::string url = "about:blank", title;
  std::map<uint64_t, std::string> captures;
  bool encoding = false;
  CefRect popup;
  bool popup_open = false;
  Tab(uint64_t id, const std::string &file) : id(id) {
    int fd = open(file.c_str(), O_RDWR | O_CLOEXEC);
    if (fd < 0)
      throw std::runtime_error("Cannot open browser surface");
    mem = (Header *)mmap(nullptr, capacity, PROT_READ | PROT_WRITE, MAP_SHARED,
                         fd, 0);
    close(fd);
    if (mem == MAP_FAILED)
      throw std::runtime_error("Cannot map browser surface");
  }
  ~Tab() override { munmap(mem, capacity); }
  CefRefPtr<CefRenderHandler> GetRenderHandler() override { return this; }
  CefRefPtr<CefLifeSpanHandler> GetLifeSpanHandler() override { return this; }
  CefRefPtr<CefLoadHandler> GetLoadHandler() override { return this; }
  CefRefPtr<CefDisplayHandler> GetDisplayHandler() override { return this; }
  CefRefPtr<CefRequestHandler> GetRequestHandler() override { return this; }
  CefRefPtr<CefDialogHandler> GetDialogHandler() override { return this; }
  CefRefPtr<CefContextMenuHandler> GetContextMenuHandler() override {
    return this;
  }
  void OnBeforeContextMenu(B, F, CefRefPtr<CefContextMenuParams>,
                           CefRefPtr<CefMenuModel> menu) override {
    menu->Clear();
  }
  bool OnFileDialog(B, FileDialogMode, const CefString &, const CefString &,
                    const std::vector<CefString> &,
                    const std::vector<CefString> &,
                    const std::vector<CefString> &,
                    CefRefPtr<CefFileDialogCallback> cb) override {
    cb->Cancel();
    return true;
  }
  void GetViewRect(B, CefRect &r) override { r = CefRect(0, 0, w, h); }
  bool GetScreenInfo(B, CefScreenInfo &s) override {
    s.device_scale_factor = scale;
    return true;
  }
  void OnPopupSize(B b, const CefRect &r) override {
    if (popup_open && (popup.x != r.x || popup.y != r.y ||
                       popup.width != r.width || popup.height != r.height))
      b->GetHost()->Invalidate(PET_VIEW); // restore the old popup footprint
    popup = r;
  }
  void OnPopupShow(B b, bool show) override {
    popup_open = show;
    if (!show)
      b->GetHost()->Invalidate(PET_VIEW);
  }
  void refresh_visibility() {
    browser->GetHost()->WasHidden(!visible && captures.empty());
    if (visible || !captures.empty())
      browser->GetHost()->Invalidate(PET_VIEW);
  }
  void cancel_captures(const char *why) {
    for (auto &[req, path] : captures)
      emit({{"req", req}, {"error", why}});
    captures.clear();
  }
  void OnPaint(B b, PaintElementType type, const RectList &, const void *pixels,
               int width, int height) override {
    if ((!visible && captures.empty()) || closing || width <= 0 ||
        height <= 0 || width > int(limit) || height > int(limit))
      return;
    if (type == PET_VIEW && b->GetHost()->GetZoomLevel() != 0) {
      b->GetHost()->SetZoomLevel(0);
      b->GetHost()->Invalidate(PET_VIEW);
      return;
    }
    // Compositor callbacks may still describe the old surface after WasResized.
    if ((type == PET_VIEW && (std::abs(width - w * scale) >= 1 ||
                              std::abs(height - h * scale) >= 1)) ||
        (type == PET_POPUP && !base_ready))
      return;
    std::atomic_ref<uint32_t> seq(mem->seq);
    auto dst = reinterpret_cast<unsigned char *>(mem) + 64;
    seq.fetch_add(1);
    if (type == PET_VIEW) {
      base_ready = true;
      mem->view_width = w;
      mem->view_height = h;
      mem->scale = scale;
      mem->width = width;
      mem->height = height;
      memcpy(dst, pixels, size_t(width) * height * 4);
    } else if (popup_open) {
      int x = std::round(popup.x * scale), y = std::round(popup.y * scale);
      int left = std::max(0, -x), right = std::min(width, int(mem->width) - x);
      for (int i = std::max(0, -y);
           right > left && i < std::min(height, int(mem->height) - y); i++)
        memcpy(dst + ((i + y) * mem->width + x + left) * 4,
               (const char *)pixels + (i * width + left) * 4,
               (right - left) * 4);
    }
    seq.fetch_add(1);
    if (visible)
      emit({{"tab", id}, {"event", "paint"}});
    if (type == PET_VIEW && popup_open)
      b->GetHost()->Invalidate(PET_POPUP);
    if (captures.empty() || encoding || encoders == 2 || b->IsLoading() ||
        (type == PET_VIEW && popup_open))
      return;
    const auto cw = mem->width, ch = mem->height;
    std::vector<unsigned char> frame(dst, dst + size_t(cw) * ch * 4);
    auto encode = [tab = CefRefPtr<Tab>(this), jobs = captures, page = url,
                   cw, ch, frame = std::move(frame)]() {
      std::map<uint64_t, Json> results;
      for (const auto &[req, path] : jobs) {
        png_image img{};
        img.version = PNG_IMAGE_VERSION;
        img.width = cw;
        img.height = ch;
        img.format = PNG_FORMAT_BGRA;
        // Rust owns the private temporary file. Never recreate it if a
        // cancellation unlinks it before this job runs.
        int fd = open(path.c_str(), O_WRONLY | O_CLOEXEC | O_NOFOLLOW);
        FILE *out = fd < 0 ? nullptr : fdopen(fd, "wb");
        if (!out) {
          results[req] = {{"req", req}, {"error", std::strerror(errno)}};
          if (fd >= 0)
            close(fd);
          continue;
        }
        bool written = png_image_write_to_stdio(&img, out, 0, frame.data(), 0, nullptr);
        int closed = fclose(out);
        if (written && closed == 0)
          results[req] = {{"req", req}, {"url", page}, {"width", cw}, {"height", ch}};
        else
          results[req] = {{"req", req}, {"error", written ? std::strerror(errno) : img.message}};
        png_image_free(&img);
      }
      CefPostTask(TID_UI, new Work([tab, results = std::move(results)] {
        for (const auto &[req, result] : results)
          if (tab->captures.erase(req)) // cancel/handoff/close may have won
            emit(result);
        tab->encoding = false;
        --encoders;
        if (tab->browser && !tab->closing && !tab->visible && tab->captures.empty())
          tab->browser->GetHost()->WasHidden(true);
        for (const auto &[id, pending] : tabs)
          if (pending->browser && !pending->encoding && !pending->closing && !pending->captures.empty())
            pending->browser->GetHost()->Invalidate(PET_VIEW);
      }));
    };
    if (CefPostTask(TID_FILE_USER_BLOCKING, new Work(std::move(encode)))) {
      encoding = true;
      ++encoders;
    } else {
      cancel_captures("Screenshot worker unavailable; retry the capture");
      refresh_visibility();
    }
  }
  void state(B b) {
    emit({{"tab", id},
          {"event", "state"},
          {"url", url},
          {"title", title},
          {"back", b->CanGoBack()},
          {"forward", b->CanGoForward()},
          {"loading", b->IsLoading()}});
  }
  void OnAddressChange(B b, F f, const CefString &u) override {
    if (f->IsMain()) {
      if (semantics && url != u.ToString())
        semantics->navigation();
      url = u.ToString();
      state(b);
    }
  }
  void OnTitleChange(B b, const CefString &t) override {
    title = t.ToString();
    state(b);
  }
  void OnTextSelectionChanged(B, const CefString &text,
                              const CefRange &) override {
    if (visible)
      emit({{"tab", id}, {"event", "selection"}, {"text", text.ToString()}});
  }
  void OnLoadingStateChange(B b, bool loading, bool, bool) override {
    state(b);
    if (!loading && !captures.empty())
      b->GetHost()->Invalidate(PET_VIEW);
  }
  void OnLoadStart(B b, F f, TransitionType) override {
    if (f->IsMain()) {
      if (semantics)
        semantics->new_document();
      // Orbit has no page zoom. Normalize any site zoom left by an older
      // Orbit build in this project profile, in Fit as well as fixed mode.
      if (b->GetHost()->GetZoomLevel() != 0)
        b->GetHost()->SetZoomLevel(0);
    }
  }
  void OnLoadError(B, F f, ErrorCode code, const CefString &text,
                   const CefString &) override {
    if (f->IsMain() && code != ERR_ABORTED)
      emit({{"tab", id}, {"event", "error"}, {"error", text.ToString()}});
  }
  void OnRenderProcessTerminated(B, TerminationStatus, int,
                                 const CefString &why) override {
    if (semantics)
      semantics->clear("Page process terminated");
    cancel_captures("Page process terminated");
    emit({{"tab", id},
          {"event", "error"},
          {"error", "Page process terminated; reload. " + why.ToString()}});
  }
  bool OnBeforeBrowse(B, F f, CefRefPtr<CefRequest> req, bool, bool) override {
    return f->IsMain() && !web(req->GetURL().ToString());
  }
  bool redirect(B b, const CefString &u) {
    if (web(u.ToString()))
      b->GetMainFrame()->LoadURL(u);
    return true;
  }
  bool OnOpenURLFromTab(B b, F, const CefString &u,
                        cef_window_open_disposition_t, bool) override {
    return redirect(b, u);
  }
  bool OnBeforePopup(B b, F, int, const CefString &u, const CefString &,
                     cef_window_open_disposition_t, bool,
                     const CefPopupFeatures &, CefWindowInfo &,
                     CefRefPtr<CefClient> &, CefBrowserSettings &,
                     CefRefPtr<CefDictionaryValue> &, bool *) override {
    return redirect(b, u);
  }
  void close_tab() {
    if (closing)
      return;
    closing = true;
    if (semantics)
      semantics->clear("Browser closed");
    cancel_captures("Browser closed");
    auto host = browser->GetHost();
    auto context = host->GetRequestContext();
    auto cookies = context ? context->GetCookieManager(nullptr) : nullptr;
    if (!cookies || !cookies->FlushStore(new Flushed(browser)))
      host->CloseBrowser(true);
  }
  void OnBeforeClose(B) override {
    cancel_captures("Browser closed");
    if (semantics)
      semantics->close();
    browser = nullptr;
    tabs.erase(id);
    emit({{"tab", id}, {"event", "closed"}});
    if (quitting && tabs.empty())
      CefQuitMessageLoop();
  }
  IMPLEMENT_REFCOUNTING(Tab);
};

static void command(Json j) {
  uint64_t req = 0, id = 0;
  std::string op;
  try {
    req = j.value("req", uint64_t(0));
    id = j.value("tab", uint64_t(0));
    op = j.at("op").get<std::string>();
    if (op == "quit") {
      quitting = true;
      if (tabs.empty())
        CefQuitMessageLoop();
      // OnBeforeClose may erase the tab synchronously.
      auto active = tabs;
      for (auto &[key, t] : active)
        if (t->browser)
          t->close_tab();
      return;
    }
    if (op == "open") {
      CefRefPtr<Tab> t = new Tab(id, j.at("file"));
      CefRequestContextSettings ctx;
      std::string profile = j.at("profile");
      if (!profile.empty())
        CefString(&ctx.cache_path) = profile;
      auto context = CefRequestContext::CreateContext(ctx, nullptr);
      CefWindowInfo wi;
      wi.SetAsWindowless(0);
      wi.runtime_style = CEF_RUNTIME_STYLE_ALLOY;
      CefBrowserSettings settings;
      settings.windowless_frame_rate = 60;
      if (!CefBrowserHost::CreateBrowser(wi, t, "about:blank", settings,
                                         nullptr, context))
        throw std::runtime_error("CEF could not open tab");
      tabs.emplace(id, t);
      return;
    }
    auto it = tabs.find(id);
    if (it == tabs.end() || it->second->closing)
      throw std::runtime_error("Browser tab is closed");
    auto t = it->second;
    if (!t->browser) {
      if (op == "control" && !j.at("allow").get<bool>())
        std::erase_if(t->deferred,
                      [](const Json &c) { return c.contains("req"); });
      if (op == "cancel")
        std::erase_if(t->deferred, [&](const Json &c) {
          return c.value("req", uint64_t(0)) == j.at("ticket");
        });
      t->deferred.push_back(std::move(j));
      return;
    }
    auto b = t->browser;
    auto host = b->GetHost();
    auto frame = b->GetMainFrame();
    if (req &&
        (!t->allowed ||
         j.at("lease") != std::atomic_ref<uint32_t>(t->mem->lease).load()))
      throw std::runtime_error("User has browser control");
    if (op == "close") {
      t->close_tab();
      return;
    } else if (op == "control") {
      t->allowed = j.at("allow");
      emit({{"tab", id}, {"event", "control"}, {"epoch", j.at("epoch")}});
      if (!t->allowed) {
        if (t->semantics)
          t->semantics->clear("User took control");
        t->cancel_captures("User took control");
        t->refresh_visibility();
      }
    } else if (op == "visible") {
      t->visible = j.at("show");
      if (!t->visible)
        host->SetFocus(false);
      t->refresh_visibility();
    } else if (op == "size") {
      t->panel_w = j.at("width");
      t->panel_h = j.at("height");
      t->panel_scale = j.at("scale");
      if (!t->fixed)
        t->size(t->panel_w, t->panel_h, t->panel_scale);
    } else if (op == "viewport") {
      const auto &fixed = j.at("fixed");
      if (!fixed.is_null()) {
        t->fixed = true;
        t->size(fixed.at(0), fixed.at(1), 1);
      } else if (t->fixed) {
        t->fixed = false;
        t->size(t->panel_w, t->panel_h, t->panel_scale);
      }
      emit({{"tab", id}, {"event", "viewport"}, {"fixed", fixed}});
      if (req)
        emit({{"req", req},
              {"url", t->url},
              {"mode", t->fixed ? "fixed" : "fit"},
              {"width", t->w},
              {"height", t->h}});
      return;
    } else if (op == "evaluate" || op == "inspect" || op == "wait" ||
               op == "activate" || op == "fill") {
      if (!t->semantics) {
        Tab *page = t.get();
        t->semantics = new Semantics(
            b,
            [page](uint32_t epoch) {
              return !page->closing && page->allowed &&
                     epoch ==
                         std::atomic_ref<uint32_t>(page->mem->lease).load();
            },
            [page] { return page->url; });
      }
      t->semantics->start(std::move(j));
      return;
    } else if (op == "navigate") {
      std::string u = j.at("url");
      if (!web(u))
        throw std::runtime_error("Expected an HTTP(S) URL");
      frame->LoadURL(u);
    } else if (op == "back")
      b->GoBack();
    else if (op == "forward")
      b->GoForward();
    else if (op == "reload")
      b->Reload();
    else if (op == "stop")
      b->StopLoad();
    else if (op == "find")
      host->Find(j.at("text").get<std::string>(), true, false,
                 j.value("next", false));
    else if (op == "end_find")
      host->StopFinding(true);
    else if (op == "focus")
      host->SetFocus(j.at("focus"));
    else if (op == "capture_lost")
      host->SendCaptureLostEvent();
    else if (op == "cut")
      frame->Cut();
    else if (op == "preedit") {
      CefString text(j.at("text").get<std::string>());
      if (text.empty())
        host->ImeCancelComposition();
      else
        host->ImeSetComposition(text, {}, CefRange::InvalidRange(),
                                CefRange(text.length(), text.length()));
    } else if (op == "keydown" || op == "keyup") {
      CefKeyEvent e;
      e.windows_key_code = j.at("code");
      e.modifiers = j.value("mods", 0);
      e.type = op == "keydown" ? KEYEVENT_RAWKEYDOWN : KEYEVENT_KEYUP;
      host->SendKeyEvent(e);
      if (op == "keydown") {
        CefString text(j.value("text", std::string()));
        for (auto c : text.ToString16()) {
          e.type = KEYEVENT_CHAR;
          e.character = c;
          e.unmodified_character = c;
          e.windows_key_code = c;
          host->SendKeyEvent(e);
        }
      }
    } else if (op == "select_all")
      frame->SelectAll();
    else if (op == "text") {
      host->SetFocus(true);
      host->ImeCommitText(j.at("text").get<std::string>(),
                          CefRange::InvalidRange(), 0);
    } else if (op == "key") {
      host->SetFocus(true);
      CefKeyEvent e;
      e.windows_key_code = j.at("code");
      e.modifiers = j.value("mods", 0);
      e.type = KEYEVENT_RAWKEYDOWN;
      host->SendKeyEvent(e);
      if (e.windows_key_code == 13 || e.windows_key_code == 9) {
        e.type = KEYEVENT_CHAR;
        e.character = e.windows_key_code;
        host->SendKeyEvent(e);
      }
      e.type = KEYEVENT_KEYUP;
      host->SendKeyEvent(e);
    } else if (op == "click" || op == "down" || op == "up" || op == "move" ||
               op == "scroll") {
      CefMouseEvent e;
      double factor = req ? t->scale : 1.;
      e.x = j.value("x", 0) / factor;
      e.y = j.value("y", 0) / factor;
      e.modifiers = j.value("mods", 0);
      if (req && (e.x < 0 || e.y < 0 || e.x >= t->w || e.y >= t->h))
        throw std::runtime_error("Point is outside the page viewport");
      if (op == "scroll")
        host->SendMouseWheelEvent(e, j.value("dx", 0), j.value("dy", 0));
      else if (op == "move")
        host->SendMouseMoveEvent(e, j.value("leave", false));
      else {
        host->SetFocus(true);
        auto flag = j.value("button", int(EVENTFLAG_LEFT_MOUSE_BUTTON));
        auto button = flag == EVENTFLAG_RIGHT_MOUSE_BUTTON ? MBT_RIGHT
                    : flag == EVENTFLAG_MIDDLE_MOUSE_BUTTON ? MBT_MIDDLE : MBT_LEFT;
        if (op != "up")
          host->SendMouseClickEvent(e, button, false, j.value("count", 1));
        if (op != "down")
          host->SendMouseClickEvent(e, button, true, j.value("count", 1));
      }
    } else if (op == "screenshot") {
      t->captures.emplace(req, j.at("file").get<std::string>());
      t->refresh_visibility();
      return;
    } else if (op == "cancel") {
      if (t->semantics)
        t->semantics->cancel(j.at("ticket"));
      t->captures.erase(j.at("ticket").get<uint64_t>());
      t->refresh_visibility();
    } else
      throw std::runtime_error("Unknown browser command");
    if (req)
      emit({{"req", req}, {"url", t->url}});
  } catch (const std::exception &err) {
    emit({{"req", req},
          {"tab", id},
          {"event",
           op == "open" ? "open_failed" : "error"},
          {"error", err.what()}});
  }
}
int main(int argc, char **argv) {
  CefMainArgs args(argc, argv);
  int sub = CefExecuteProcess(args, nullptr, nullptr);
  if (sub >= 0)
    return sub;
  // Keep protocol output separate from CEF and native-library diagnostics.
  events = fdopen(fcntl(STDOUT_FILENO, F_DUPFD_CLOEXEC, 3), "w");
  if (!events || dup2(STDERR_FILENO, STDOUT_FILENO) == -1)
    return 2;
  CefSettings settings;
  settings.windowless_rendering_enabled = true;
  const char *root = getenv("ORBIT_BROWSER_ROOT");
  if (!root)
    return 2;
  CefString(&settings.root_cache_path) = root;
  if (!CefInitialize(args, settings, nullptr, nullptr))
    return 3;
  std::thread input([] {
    for (std::string s; std::getline(std::cin, s);) {
      auto message = Json::parse(s, nullptr, false);
      if (!message.is_object()) {
        std::cerr << "Ignoring invalid Orbit browser command\n";
        continue;
      }
      if (message.contains("op") && message["op"] == "quit")
        break;
      CefPostTask(TID_UI, new Work([j = std::move(message)] { command(j); }));
    }
    CefPostTask(TID_UI, new Work([] { command({{"op", "quit"}}); }));
  });
  CefRunMessageLoop();
  input.join();
  CefShutdown();
}
