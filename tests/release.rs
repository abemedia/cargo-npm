#[allow(dead_code)]
mod testenv;
use testenv::TestEnv;

use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn tgz(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    let mut ar = tar::Builder::new(gz);
    for (name, data) in entries {
        let mut header = tar::Header::new_gnu();
        header.set_size(data.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        ar.append_data(&mut header, name, *data).unwrap();
    }
    ar.into_inner().unwrap().finish().unwrap()
}

async fn serve(server: &MockServer, asset_path: &str, body: Vec<u8>) {
    Mock::given(method("HEAD"))
        .and(path(asset_path))
        .respond_with(ResponseTemplate::new(200))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path(asset_path))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(body))
        .mount(server)
        .await;
}

fn env_with_release(server: &MockServer, extra: &toml::Table) -> TestEnv {
    let mut config = toml::toml! {
        [package]
        repository = (format!("{}/owner/my-tool", server.uri()))

        [package.metadata.npm]
        targets = ["x86_64-unknown-linux-gnu", "aarch64-apple-darwin"]
        pkg-url = "{ repo }/releases/download/v{ version }/{ name }-{ target }.tar.gz"
    };
    if let Some(toml::Value::Table(npm)) = extra.get("npm") {
        config["package"]["metadata"]["npm"]
            .as_table_mut()
            .unwrap()
            .extend(npm.clone());
    }
    TestEnv::package_with_config(config)
}

#[tokio::test]
async fn from_release_downloads_and_installs_binaries() {
    let server = MockServer::start().await;
    for triple in ["x86_64-unknown-linux-gnu", "aarch64-apple-darwin"] {
        serve(
            &server,
            &format!("/owner/my-tool/releases/download/v1.0.0/my-tool-{triple}.tar.gz"),
            tgz(&[
                (&format!("my-tool-{triple}/my-tool"), triple.as_bytes()),
                (&format!("my-tool-{triple}/README.md"), b"readme"),
            ]),
        )
        .await;
    }

    let env = env_with_release(&server, &toml::Table::new());
    env.assert_ok("generate", &["--from-release"]);
    env.assert_generated(&["my-tool", "my-tool-linux-x64", "my-tool-darwin-arm64"]);
    assert_eq!(
        env.read_file("npm/my-tool-linux-x64/my-tool"),
        "x86_64-unknown-linux-gnu"
    );
    assert_eq!(
        env.read_file("npm/my-tool-darwin-arm64/my-tool"),
        "aarch64-apple-darwin"
    );
    env.assert_not_exists("npm/my-tool-linux-x64/README.md");
    env.assert_not_exists("npm/.tmp");
}

#[tokio::test]
async fn from_release_errors_when_binary_is_not_at_expected_path() {
    let server = MockServer::start().await;
    for triple in ["x86_64-unknown-linux-gnu", "aarch64-apple-darwin"] {
        serve(
            &server,
            &format!("/owner/my-tool/releases/download/v1.0.0/my-tool-{triple}.tar.gz"),
            tgz(&[("somewhere/else/my-tool", b"bin")]),
        )
        .await;
    }

    let env = env_with_release(&server, &toml::Table::new());
    env.assert_err(
        "generate",
        &["--from-release"],
        "binary 'my-tool' not found at my-tool",
    );
    env.assert_not_exists("npm/.tmp");
}

#[tokio::test]
async fn from_release_uses_bin_dir_override() {
    let server = MockServer::start().await;
    for triple in ["x86_64-unknown-linux-gnu", "aarch64-apple-darwin"] {
        serve(
            &server,
            &format!("/owner/my-tool/releases/download/v1.0.0/my-tool-{triple}.tar.gz"),
            tgz(&[("somewhere/else/my-tool", b"bin")]),
        )
        .await;
    }

    let env = env_with_release(
        &server,
        &toml::toml! {
            [npm]
            bin-dir = "somewhere/else/{ bin }{ binary-ext }"
        },
    );
    env.assert_ok("generate", &["--from-release"]);
    assert_eq!(env.read_file("npm/my-tool-linux-x64/my-tool"), "bin");
}

#[tokio::test]
async fn from_release_errors_when_asset_is_missing() {
    let server = MockServer::start().await;
    serve(
        &server,
        "/owner/my-tool/releases/download/v1.0.0/my-tool-x86_64-unknown-linux-gnu.tar.gz",
        tgz(&[("my-tool", b"bin")]),
    )
    .await;

    let env = env_with_release(&server, &toml::Table::new());
    env.assert_err(
        "generate",
        &["--from-release"],
        "no release asset found for aarch64-apple-darwin",
    );
}

#[test]
fn from_release_rejects_infer_targets() {
    let env = TestEnv::package_with_config(toml::toml! {
        [package]
        repository = "https://github.com/owner/my-tool"
    });
    env.assert_err(
        "generate",
        &["--from-release", "--infer-targets"],
        "--infer-targets is not supported with --from-release",
    );
}

#[test]
fn from_release_requires_repository() {
    let env = TestEnv::package_with_config(toml::toml! {
        [package]
        repository = ""
    });
    env.assert_err(
        "generate",
        &["--from-release"],
        "--from-release needs `repository`",
    );
}
