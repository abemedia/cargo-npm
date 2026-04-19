use std::io::{BufRead, Read, Write};
use std::net::TcpStream;
use std::sync::mpsc;
use std::time::{Duration, Instant};
use tempfile::TempDir;

struct ChildGuard(std::process::Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        self.0.kill().ok();
        self.0.wait().ok();
    }
}

const CARGO_NPM: &str = env!("CARGO_BIN_EXE_cargo-npm");

#[test]
#[ignore = "requires npm/npx; run with: cargo test --test e2e -- --ignored --nocapture"]
#[allow(clippy::too_many_lines)]
fn e2e_build_publish_install_run() {
    // Platform-specific npm/npx command names
    let (npm, npx) = if cfg!(windows) {
        ("npm.cmd", "npx.cmd")
    } else {
        ("npm", "npx")
    };

    // Start Verdaccio

    let verdaccio_dir = TempDir::new().unwrap();
    let port = free_port();
    let storage_path = verdaccio_dir.path().join("storage");
    let htpasswd_path = verdaccio_dir.path().join("htpasswd");
    let config_path = verdaccio_dir.path().join("config.yaml");

    std::fs::create_dir_all(&storage_path).unwrap();
    std::fs::write(&htpasswd_path, "").unwrap();
    std::fs::write(
        &config_path,
        [
            format!("storage: {}", storage_path.display()),
            "auth:".into(),
            "  htpasswd:".into(),
            format!("    file: {}", htpasswd_path.display()),
            "    max_users: 10".into(),
            "uplinks: {}".into(),
            "packages:".into(),
            "  '**':".into(),
            "    access: $all".into(),
            "    publish: $authenticated".into(),
            "    proxy: []".into(),
            format!("listen: 127.0.0.1:{port}"),
        ]
        .join("\n"),
    )
    .unwrap();

    eprintln!("[e2e] spawning verdaccio on port {port}");
    let mut verdaccio = std::process::Command::new(npx)
        .args(["--yes", "verdaccio@6", "--config"])
        .arg(&config_path)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("failed to spawn verdaccio");
    let stdout = verdaccio.stdout.take().unwrap();
    let stderr = verdaccio.stderr.take().unwrap();

    // Ensure verdaccio is killed on drop, even if the test panics.
    let _guard = ChildGuard(verdaccio);

    // Both stdout and stderr are forwarded to a single channel so we can watch
    // for the readiness line and accumulate a combined log for error reporting.
    let (tx, rx) = mpsc::channel::<String>();
    let tx2 = tx.clone();
    std::thread::spawn(move || {
        for line in std::io::BufReader::new(stdout)
            .lines()
            .map_while(Result::ok)
        {
            tx2.send(line).ok();
        }
    });
    std::thread::spawn(move || {
        for line in std::io::BufReader::new(stderr)
            .lines()
            .map_while(Result::ok)
        {
            tx.send(line).ok();
        }
    });

    let deadline = Instant::now() + Duration::from_mins(3);
    let mut log = String::new();
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        assert!(
            !remaining.is_zero(),
            "verdaccio did not start within 3m on port {port}\n{log}"
        );
        match rx.recv_timeout(remaining) {
            Ok(line) if line.contains("http address") => break,
            Ok(line) => {
                log.push_str(&line);
                log.push('\n');
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                panic!("verdaccio did not print 'http address' within 3m on port {port}\n{log}")
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                panic!("verdaccio closed before becoming ready\n{log}")
            }
        }
    }
    eprintln!("[e2e] verdaccio ready");

    // Register a test user and get an auth token via the npm registry API.

    let registry_url = format!("http://127.0.0.1:{port}");

    let body = serde_json::json!({
        "name": "test", "password": "test",
        "email": "test@example.com", "type": "user",
    })
    .to_string();
    let request = format!(
        "PUT /-/user/org.couchdb.user:test HTTP/1.0\r\n\
         Host: 127.0.0.1:{port}\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {len}\r\n\
         \r\n\
         {body}",
        len = body.len(),
    );
    let addr: std::net::SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(5)).unwrap();
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
    stream.write_all(request.as_bytes()).unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    let body_start = response
        .find("\r\n\r\n")
        .expect("invalid HTTP response from verdaccio");
    let json: serde_json::Value =
        serde_json::from_str(&response[body_start + 4..]).expect("invalid JSON from verdaccio");
    let token = json["token"]
        .as_str()
        .expect("no token in verdaccio response")
        .to_string();

    // Build a real "hello world" Rust binary

    let pkg_dir = TempDir::new().unwrap();

    std::fs::write(
        pkg_dir.path().join("Cargo.toml"),
        toml::toml! {
            [package]
            name = "my-tool"
            version = "1.2.3"
            edition = "2021"
            license = "MIT"
            description = "A test tool"
            repository = "https://github.com/example/my-tool"

            [[bin]]
            name = "my-tool"
            path = "src/bin/my-tool.rs"
        }
        .to_string(),
    )
    .unwrap();

    std::fs::create_dir_all(pkg_dir.path().join("src/bin")).unwrap();
    std::fs::write(
        pkg_dir.path().join("src/bin/my-tool.rs"),
        "fn main() { println!(\"hello, cargo-npm!\"); }",
    )
    .unwrap();

    eprintln!("[e2e] running cargo build --release");
    let out = std::process::Command::new("cargo")
        .args(["build", "--release"])
        .current_dir(pkg_dir.path())
        .env("CARGO_TARGET_DIR", pkg_dir.path().join("target"))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "cargo build failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let scope = registry_url.trim_start_matches("http:");
    std::fs::write(
        pkg_dir.path().join(".npmrc"),
        format!("registry={registry_url}/\n{scope}/:_authToken={token}\n"),
    )
    .unwrap();

    eprintln!("[e2e] running cargo npm generate");
    let out = std::process::Command::new(CARGO_NPM)
        .args(["npm", "generate", "--infer-targets"])
        .current_dir(pkg_dir.path())
        .env("CARGO_TARGET_DIR", pkg_dir.path().join("target"))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "cargo npm generate failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );

    eprintln!("[e2e] running cargo npm publish");
    let out = std::process::Command::new(CARGO_NPM)
        .args(["npm", "publish"])
        .current_dir(pkg_dir.path())
        .env("CARGO_TARGET_DIR", pkg_dir.path().join("target"))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "cargo npm publish failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );

    eprintln!("{}", String::from_utf8_lossy(&out.stdout));

    // npm init, install, and run

    let install_dir = TempDir::new().unwrap();
    let project_dir = install_dir.path().join("project");
    std::fs::create_dir_all(&project_dir).unwrap();

    std::fs::write(
        project_dir.join(".npmrc"),
        format!("registry={registry_url}/\n"),
    )
    .unwrap();

    eprintln!("[e2e] npm init");
    let out = std::process::Command::new(npm)
        .args(["init", "-y"])
        .current_dir(&project_dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "npm init failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );

    eprintln!("[e2e] npm install my-tool");
    let out = std::process::Command::new(npm)
        .args(["install", "my-tool"])
        .current_dir(&project_dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "npm install failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );

    eprintln!("[e2e] npm exec my-tool");
    let out = std::process::Command::new(npm)
        .args(["exec", "my-tool"])
        .current_dir(&project_dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "npm exec failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );

    let stdout = String::from_utf8_lossy(&out.stdout);
    eprintln!("[e2e] output: {stdout}");
    assert!(
        stdout.contains("hello, cargo-npm!"),
        "unexpected output: {stdout}"
    );
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}
