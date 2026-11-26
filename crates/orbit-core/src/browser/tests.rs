use super::*;
use std::process::Command;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[test]
fn address_boundary() {
    assert_eq!(
        address(" localhost:8000/path ").unwrap(),
        "http://localhost:8000/path"
    );
    assert_eq!(address("example.org").unwrap(), "https://example.org/");
    for url in [
        "file:///etc/hosts",
        "chrome://settings",
        "https://user:secret@example.org",
        "",
    ] {
        assert!(address(url).is_err(), "{url}");
    }
}
fn worktrees(dir: &Path) -> (PathBuf, PathBuf) {
    let main = dir.join("repo");
    let tree = main.join("nested-worktree");
    fs::create_dir(&main).unwrap();
    for args in [
        vec!["init", "-q"],
        vec![
            "-c",
            "user.name=Orbit",
            "-c",
            "user.email=test@localhost",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--allow-empty",
            "-qm",
            "init",
        ],
        vec!["worktree", "add", "-q", "--detach", tree.to_str().unwrap()],
    ] {
        assert!(
            Command::new("git")
                .current_dir(&main)
                .args(args)
                .status()
                .unwrap()
                .success()
        );
    }
    (main, tree)
}
#[test]
fn worktree_profile_uses_registered_checkout_and_relative_project() {
    let home = tempfile::tempdir().unwrap();
    let (main, tree) = worktrees(home.path());
    assert_eq!(profile_key(&tree).unwrap(), profile_key(&main).unwrap());
    for root in [&main, &tree] {
        fs::create_dir(root.join("app")).unwrap();
    }
    assert_eq!(
        profile_key(&tree.join("app")).unwrap(),
        profile_key(&main.join("app")).unwrap()
    );
    assert_ne!(
        profile_key(&main).unwrap(),
        profile_key(&main.join("app")).unwrap()
    );
}
async fn fixture() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            tokio::spawn(async move {
                let mut request = Vec::new();
                let mut buf = [0; 2048];
                while !request.windows(4).any(|s| s == b"\r\n\r\n") {
                    let n = socket.read(&mut buf).await.unwrap();
                    if n == 0 {
                        return;
                    }
                    request.extend(&buf[..n]);
                }
                let req = String::from_utf8_lossy(&request);
                let seed = req.starts_with("GET /seed ");
                let cookie = req.lines().any(|l| {
                    l.to_ascii_lowercase().starts_with("cookie:") && l.contains("login=orbit")
                });
                let color = if seed || cookie { "#00ff00" } else { "#ff0000" };
                let body = format!(
                    "<style>body{{background:{color};height:300vh;margin:0}}input,button{{height:40px;width:100px;box-sizing:border-box}}</style><input id=q><button onclick=\"if(q.value==='Orbitβ')document.body.style.background='blue'\">Apply</button><script>onscroll=()=>document.body.style.background='yellow'</script>"
                );
                let set = if seed {
                    "Set-Cookie: login=orbit; Max-Age=3600; Path=/; HttpOnly\r\n"
                } else {
                    ""
                };
                let _=socket.write_all(format!("HTTP/1.1 200 OK\r\n{set}Content-Type: text/html; charset=utf-8\r\nCache-Control: no-store\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",body.len()).as_bytes()).await;
            });
        }
    });
    format!("http://{addr}")
}
async fn act(t: &AgentTab, v: Value) -> Result<(Value, Option<Vec<u8>>)> {
    t.call(&v.to_string(), &CancellationToken::new()).await
}
async fn see(t: &AgentTab, url: &str, rgb: [u8; 3]) {
    let mut last = None;
    for _ in 0..30 {
        let (v, png) = act(t, json!({"action":"screenshot"})).await.unwrap();
        let img = image::load_from_memory(&png.unwrap()).unwrap().to_rgb8();
        let pixel = img.get_pixel(240, 150).0;
        if v["url"] == url && pixel == rgb {
            return;
        }
        last = Some((v, pixel));
        tokio::time::sleep(Duration::from_millis(80)).await;
    }
    panic!("Tab {} expected {url}, {rgb:?}; saw {last:?}", t.id);
}
/// Exercise real sandboxed renderers; no user profiles, credentials or network dependency.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires packaged CEF helper and Linux renderer sandbox; run just browser-smoke"]
async fn native_browser_session_lifecycle() {
    let dir = tempfile::tempdir().unwrap();
    let (main, tree) = worktrees(dir.path());
    let root = crate::store::Store::open(dir.path().join("state")).unwrap();
    let b = Browser::new(root.clone()).unwrap();
    let session = |path: &Path| {
        let session = crate::store::Session::new(path.into(), &Default::default());
        root.save_session(&session).unwrap();
        AgentTab {
            browser: b.clone(),
            id: session.id,
            project: path.into(),
        }
    };
    let a = session(&main);
    let other = session(&tree);
    let server = fixture().await;
    let seed = format!("{server}/seed");
    let check = format!("{server}/check");
    act(&a, json!({"action":"navigate","url":seed}))
        .await
        .unwrap();
    see(&a, &seed, [0, 255, 0]).await;
    b.show(&a.id, &main).unwrap();
    act(&a, json!({"action":"click","x":40,"y":18}))
        .await
        .unwrap();
    act(&a, json!({"action":"type","text":"Orbitβ"}))
        .await
        .unwrap();
    act(&a, json!({"action":"click","x":155,"y":18}))
        .await
        .unwrap();
    see(&a, &seed, [0, 0, 255]).await;
    assert!(b.frame(&a.id, None).unwrap().is_some());
    b.hide(&a.id);
    b.control(&a.id, true).unwrap();
    for v in [
        json!({"action":"screenshot"}),
        json!({"action":"navigate","url":check}),
    ] {
        assert!(
            act(&a, v)
                .await
                .unwrap_err()
                .to_string()
                .contains("control")
        );
    }
    act(&other, json!({"action":"navigate","url":check}))
        .await
        .unwrap();
    see(&other, &check, [0, 255, 0]).await;
    b.control(&a.id, false).unwrap();
    see(&a, &seed, [0, 0, 255]).await;
    act(&a, json!({"action":"scroll","x":240,"y":150,"dy":-300}))
        .await
        .unwrap();
    see(&a, &seed, [255, 255, 0]).await;
    let private = session(&main);
    b.set_isolated(&private.id, true).unwrap();
    let unrelated = session(dir.path());
    for tab in [&private, &unrelated] {
        act(tab, json!({"action":"navigate","url":check}))
            .await
            .unwrap();
        see(tab, &check, [255, 0, 0]).await;
    }
    b.close(&a.id);
    see(&other, &check, [0, 255, 0]).await;
    b.control(&a.id, true).unwrap();
    b.shutdown().unwrap();
    let fresh = AgentTab {
        browser: Browser::new(root).unwrap(),
        id: "after-restart".into(),
        project: tree,
    };
    act(&fresh, json!({"action":"navigate","url":check}))
        .await
        .unwrap();
    see(&fresh, &check, [0, 255, 0]).await;
    let locked = AgentTab {
        browser: fresh.browser.clone(),
        ..a
    };
    assert!(
        act(&locked, json!({"action":"screenshot"}))
            .await
            .unwrap_err()
            .to_string()
            .contains("control")
    );
    let incognito = AgentTab {
        browser: fresh.browser.clone(),
        ..private
    };
    act(&incognito, json!({"action":"navigate","url":check}))
        .await
        .unwrap();
    see(&incognito, &check, [255, 0, 0]).await;
    fresh.browser.shutdown().unwrap();
}
