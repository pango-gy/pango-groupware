//! `pango-groupware` 실행 파일.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use clap::{Parser, Subcommand};
use pango_core::time::SystemClock;
use pango_db::{Db, DbOptions};
use pango_server::service::setup::{self, InitInput};
use pango_server::{AppState, Config};

#[derive(Parser)]
#[command(
    name = "pango-groupware",
    version,
    about = "가볍고 직접 설치해 쓰는 사내 그룹웨어"
)]
struct Cli {
    /// 데이터(SQLite 파일)를 둘 디렉터리.
    #[arg(long, env = "PANGO_DATA_DIR", default_value = "./data", global = true)]
    data_dir: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// 회사 정보와 최고 관리자 계정을 만든다(최초 1회).
    Init {
        #[arg(long)]
        company: String,
        #[arg(long, default_value = "Asia/Seoul")]
        timezone: String,
        /// 최고 관리자 로그인 아이디.
        #[arg(long, default_value = "admin")]
        owner_login: String,
        #[arg(long, default_value = "관리자")]
        owner_name: String,
        /// 최고 관리자 비밀번호. 생략하면 임시 비밀번호를 만들어 한 번 보여 준다.
        /// 명령줄 기록에 남지 않도록 환경 변수로만 받는다.
        #[arg(skip = std::env::var("PANGO_OWNER_PASSWORD").ok())]
        owner_password: Option<String>,
    },
    /// 데이터베이스(첨부파일 포함)의 사본을 만든다. 서버가 도는 중에도 된다.
    ///
    /// 복구는 서버를 멈추고 데이터 디렉터리의 pango.db를 사본으로 바꾼 뒤(-wal·-shm 파일은 지운다)
    /// 다시 실행한다.
    Backup {
        /// 사본 파일 경로. 이미 있으면 덮어쓰지 않고 실패한다.
        dest: PathBuf,
    },
    /// 최고 관리자 계정을 되살린다: 임시 비밀번호를 주고 2단계 인증을 끄고 모든 로그인을 끊는다.
    ///
    /// 최고 관리자가 비밀번호나 인증 앱을 잃었을 때 쓴다(웹에서는 다른 사람이 최고 관리자를
    /// 초기화할 수 없다). 서버가 도는 중에도 된다.
    ResetOwner {
        /// 최고 관리자 로그인 아이디.
        login_id: String,
    },
    /// 서버를 실행한다.
    Serve {
        #[arg(long, env = "PANGO_BIND", default_value = "127.0.0.1:8080")]
        bind: SocketAddr,
        /// 앞단 리버스 프록시의 X-Forwarded-For를 신뢰한다.
        #[arg(long, env = "PANGO_TRUST_PROXY")]
        trust_proxy: bool,
        /// 서비스의 공개 주소(예: https://erp.example.com). 리버스 프록시 뒤에서는 지정을 권장한다.
        #[arg(long, env = "PANGO_PUBLIC_ORIGIN")]
        public_origin: Option<String>,
        /// HTTPS 없이 쓰는 로컬 개발용. 세션 쿠키의 Secure 속성을 끈다.
        #[arg(long)]
        insecure_cookies: bool,
        /// 외부 알림 웹훅을 사설망·루프백 주소(같은 VPC의 수신 서버 등)로도 보낸다.
        /// 클라우드 메타데이터·link-local 주소는 이 옵션을 켜도 막는다.
        #[arg(long, env = "PANGO_OUTBOUND_ALLOW_PRIVATE")]
        outbound_allow_private: bool,
        /// 첨부파일(공지·결재 문서) 하나의 최대 크기(MiB). 파일은 DB에 저장되므로 DB 크기도 함께 늘어난다.
        #[arg(long, env = "PANGO_MAX_ATTACHMENT_MB", default_value_t = 10,
              value_parser = clap::value_parser!(u32).range(1..=100))]
        max_attachment_mb: u32,
        /// AI 앱(Claude 등)이 연결하는 원격 MCP(`{공개 주소}/mcp`)와 OAuth 인가 서버를 켠다.
        /// 공개 주소(--public-origin)가 필요하다. 사용법은 docs/MCP.md.
        #[arg(long, env = "PANGO_MCP")]
        mcp: bool,
        /// MCP 로그인 후 이 컴퓨터(localhost)로 돌아오는 클라이언트(Claude Code·Desktop)도 받는다.
        #[arg(long, env = "PANGO_MCP_ALLOW_LOOPBACK")]
        mcp_allow_loopback: bool,
        /// 사내 메신저 Buzz 릴레이 주소(예: https://buzz.example.com). 봇 키와 함께 지정한다.
        #[arg(long, env = "PANGO_BUZZ_RELAY_URL")]
        buzz_relay_url: Option<String>,
        /// Buzz 봇 비밀키(nsec 또는 64자 16진수)가 든 파일. Secrets Manager 마운트 등에 쓴다.
        #[arg(long, env = "PANGO_BUZZ_BOT_KEY_FILE")]
        buzz_bot_key_file: Option<PathBuf>,
        /// Buzz 봇 비밀키. 명령줄 기록에 남지 않도록 환경 변수로만 받는다.
        #[arg(skip = std::env::var("PANGO_BUZZ_BOT_KEY").ok())]
        buzz_bot_key: Option<String>,
    },
}

type AnyError = Box<dyn std::error::Error + Send + Sync>;

async fn open_state(data_dir: &std::path::Path, config: Config) -> Result<AppState, AnyError> {
    std::fs::create_dir_all(data_dir)?;
    let db = Db::open(&data_dir.join("pango.db"), DbOptions::default()).await?;
    Ok(AppState::new(db, Arc::new(SystemClock), config)?)
}

async fn run(cli: Cli) -> Result<(), AnyError> {
    match cli.command {
        Command::Init {
            company,
            timezone,
            owner_login,
            owner_name,
            owner_password,
        } => {
            let state = open_state(&cli.data_dir, Config::default()).await?;
            let output = setup::initialize(
                &state,
                InitInput {
                    company_name: company,
                    timezone,
                    owner_login_id: owner_login.clone(),
                    owner_name,
                    owner_password,
                },
            )
            .await?;
            println!("초기화가 끝났습니다.");
            println!("  로그인 아이디: {owner_login}");
            if let Some(password) = output.temporary_password {
                println!("  임시 비밀번호: {password}");
                println!("임시 비밀번호는 다시 볼 수 없습니다. 첫 로그인 때 바꿔야 합니다.");
            }
            Ok(())
        }
        Command::Backup { dest } => {
            let path = cli.data_dir.join("pango.db");
            if !path.exists() {
                return Err(format!("데이터베이스가 없습니다: {}", path.display()).into());
            }
            // 마이그레이션을 돌리지 않는다(판올림 전 백업이 운영 DB를 바꾸지 않게).
            pango_db::backup(&path, &dest).await?;
            println!("백업을 만들었습니다: {}", dest.display());
            Ok(())
        }
        Command::ResetOwner { login_id } => {
            let path = cli.data_dir.join("pango.db");
            if !path.exists() {
                return Err(format!("데이터베이스가 없습니다: {}", path.display()).into());
            }
            let state = open_state(&cli.data_dir, Config::default()).await?;
            let output = setup::recover_owner(&state, &login_id).await?;
            println!("최고 관리자 계정을 되살렸습니다.");
            println!("  로그인 아이디: {login_id}");
            println!("  임시 비밀번호: {}", output.temporary_password);
            if output.totp_removed {
                println!("  2단계 인증을 껐습니다. 로그인한 뒤 다시 켜 주세요.");
            }
            println!("  끊은 로그인: {}개", output.revoked_sessions);
            println!("임시 비밀번호는 다시 볼 수 없습니다. 첫 로그인 때 바꿔야 합니다.");
            Ok(())
        }
        Command::Serve {
            bind,
            trust_proxy,
            public_origin,
            insecure_cookies,
            outbound_allow_private,
            max_attachment_mb,
            mcp,
            mcp_allow_loopback,
            buzz_relay_url,
            buzz_bot_key_file,
            buzz_bot_key,
        } => {
            let buzz = pango_server::config::BuzzConfig::from_settings(
                buzz_relay_url.as_deref(),
                buzz_bot_key.as_deref(),
                buzz_bot_key_file.as_deref(),
            )?;
            if let Some(buzz) = &buzz {
                tracing::info!(relay = %buzz.relay_url, bot = %buzz.bot_key.public_key().to_npub(), "Buzz channel configured");
            }
            let mcp = if mcp {
                Some(pango_server::config::McpConfig::from_public_origin(
                    public_origin.as_deref(),
                    mcp_allow_loopback,
                )?)
            } else {
                None
            };
            if let Some(mcp) = &mcp {
                tracing::info!(endpoint = %mcp.resource(), "remote MCP enabled");
            }
            let config = Config {
                buzz,
                mcp,
                trust_proxy,
                public_origin,
                cookie_secure: !insecure_cookies,
                outbound_allow_private,
                max_attachment_bytes: max_attachment_mb as usize * 1024 * 1024,
                ..Config::default()
            };
            let state = open_state(&cli.data_dir, config).await?;
            if !setup::is_initialized(&state).await? {
                return Err(
                    "초기화되지 않았습니다. 먼저 `pango-groupware init`을 실행하세요.".into(),
                );
            }
            // 판올림으로 새로 생긴 기본 결재 양식을 채운다(이미 있는 양식은 건드리지 않는다).
            let added = pango_server::service::form_catalog::ensure_form_catalog(&state).await?;
            if !added.is_empty() {
                tracing::info!(count = added.len(), "added default approval forms");
            }
            let tasks = [
                tokio::spawn(pango_server::service::maintenance::run_forever(
                    state.clone(),
                    std::time::Duration::from_secs(3600),
                )),
                // 퇴근 누락 알림(업무 종료 1시간 뒤). 5분마다 확인한다.
                tokio::spawn(pango_server::service::reminders::run_forever(
                    state.clone(),
                    std::time::Duration::from_secs(300),
                )),
                // 외부 알림(웹훅) 발송. 연동이 꺼져 있으면 보낼 일이 생기지 않는다.
                tokio::spawn(pango_server::service::outbound::run_forever(
                    state.clone(),
                    std::time::Duration::from_secs(5),
                )),
            ];
            let db = state.db.clone();
            let app = pango_server::http::router(state)
                .into_make_service_with_connect_info::<SocketAddr>();
            let listener = tokio::net::TcpListener::bind(bind).await?;
            tracing::info!(%bind, "listening");
            let served = axum::serve(listener, app)
                .with_graceful_shutdown(shutdown_signal())
                .await;
            for task in &tasks {
                task.abort();
            }
            for task in tasks {
                let _ = task.await;
            }
            db.close().await;
            tracing::info!("shutdown complete");
            served?;
            Ok(())
        }
    }
}

/// 종료 신호(Ctrl-C 또는 Unix의 SIGTERM)를 기다린다.
///
/// Docker(`docker stop`), systemd(`systemctl stop`), AWS ECS 태스크 종료는 모두 SIGTERM을
/// 보낸다. 이 신호를 받아 진행 중인 요청을 마친 뒤 DB 연결을 닫고 내려간다.
async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(error) = tokio::signal::ctrl_c().await {
            tracing::warn!(%error, "failed to listen for Ctrl-C");
            std::future::pending::<()>().await;
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(error) => {
                tracing::warn!(%error, "failed to listen for SIGTERM");
                std::future::pending::<()>().await;
            }
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => tracing::info!("received SIGINT, shutting down"),
        () = terminate => tracing::info!("received SIGTERM, shutting down"),
    }
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,sqlx=warn".into()),
        )
        .init();
    match run(Cli::parse()).await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("오류: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}
