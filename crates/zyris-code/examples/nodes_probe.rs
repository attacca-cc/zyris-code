//! 진단용. **같은 자격으로 두 번 붙으면 서버가 노드를 갈라 주는가**를 잰다.
//!
//! 이 물음이 zyris-code의 창 여럿 설계를 통째로 정한다. 서버가 연결마다 다른 `node_id`를
//! 내주면 창은 그냥 각자 붙으면 되고(`sibling.rs`의 형제 노드 찍어내기가 통째로 필요 없다),
//! 같은 `node_id`를 내주면 나중에 붙은 쪽이 먼저 붙은 쪽의 도구 호출을 통째로 가져간다.
//!
//! **추측하지 말 것.** attacca가 `register_node`를 `MethodNotFound`로 답하는 것은 확인했고
//! (2026-08-03), 그러면 남은 길은 이것뿐이라 여기서 실제로 재 본다.
//!
//! ```bash
//! ZYRIS_PROFILE=zyris-code cargo run -p zyris-code --example nodes_probe
//! ```
//!
//! `node_id`가 **서버가 ack로 준 값**이라는 것이 요점이다(`connection.rs`의 `ack.node_id`).

use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use zyris::runtime::{credentials::Credentials, RunConfig, Runner};
use zyris::NodeKind;

/// 둘째 연결이 첫째를 밀어내는지 보려면 첫째가 살아 있는 동안 붙어야 한다.
const OVERLAP: Duration = Duration::from_secs(12);

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt().with_env_filter("nodes_probe=info,zyris=warn").init();

    // 앱과 **같은 자격 파일**을 봐야 한다. `main.rs`가 하는 것과 같은 자리다.
    if std::env::var_os("ZYRIS_CONFIG_DIR").is_none() {
        if let Some(dir) = zyris_code::conn::credential_dir() {
            std::env::set_var("ZYRIS_CONFIG_DIR", dir);
        }
    }
    if std::env::var_os("ZYRIS_PROFILE").is_none() {
        std::env::set_var("ZYRIS_PROFILE", zyris_code::conn::APP);
    }
    if std::env::var_os("ZYRIS_NODE_NAME").is_none() {
        std::env::set_var("ZYRIS_NODE_NAME", zyris_code::conn::node_name());
    }

    let config = RunConfig::from_env();
    let bridge = zyris_code::tools::bridge::Bridge::new();
    let creds: Arc<dyn Credentials> = match zyris_code::enroll::source(&config, &bridge) {
        Ok((creds, _)) => creds,
        Err(e) => {
            println!("자격을 만들지 못했다: {e}");
            return ExitCode::FAILURE;
        }
    };

    // 자격 하나를 둘이 나눠 쓴다 — 창 둘이 같은 파일을 보는 것과 같은 모양이다.
    let first = dial("첫째", creds.clone());
    tokio::time::sleep(Duration::from_secs(3)).await;
    let second = dial("둘째", creds.clone());

    tokio::time::sleep(OVERLAP).await;
    println!("\n두 연결이 겹쳐 있는 동안의 결과다. 위 두 줄의 node_id를 견줘 볼 것:");
    println!("  다르면  → 서버가 연결마다 노드를 갈라 준다 (창마다 그냥 붙으면 된다)");
    println!("  같으면  → 한 노드를 두고 싸운다 (나중 것이 도구 호출을 가져간다)");
    first.abort();
    second.abort();
    ExitCode::SUCCESS
}

fn dial(label: &'static str, creds: Arc<dyn Credentials>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let runner = Runner::new(RunConfig::from_env(), creds).kind(NodeKind::Service).on_connect(
            move |conn| async move {
                let info = conn.info();
                println!("{label}: node_id={} conn_id={}", info.node_id, info.conn_id);
                // 붙은 채로 있어야 겹친다. 끊으면 밀어내는지 볼 수가 없다.
                conn.closed().await;
                println!("{label}: 연결이 끊겼다");
            },
        );
        let _ = runner.try_run().await;
    })
}
