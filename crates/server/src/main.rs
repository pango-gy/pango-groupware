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
        Command::Serve {
            bind,
            trust_proxy,
            public_origin,
            insecure_cookies,
            outbound_allow_private,
        } => {
            let config = Config {
                trust_proxy,
                public_origin,
                cookie_secure: !insecure_cookies,
                outbound_allow_private,
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
            tokio::spawn(pango_server::service::maintenance::run_forever(
                state.clone(),
                std::time::Duration::from_secs(3600),
            ));
            // 외부 알림(웹훅) 발송. 연동이 꺼져 있으면 보낼 일이 생기지 않는다.
            tokio::spawn(pango_server::service::outbound::run_forever(
                state.clone(),
                std::time::Duration::from_secs(5),
            ));
            let app = pango_server::http::router(state)
                .into_make_service_with_connect_info::<SocketAddr>();
            let listener = tokio::net::TcpListener::bind(bind).await?;
            tracing::info!(%bind, "listening");
            axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _ = tokio::signal::ctrl_c().await;
                })
                .await?;
            Ok(())
        }
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
