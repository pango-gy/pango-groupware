# 배포·운영 가이드

서버는 실행 파일 하나(웹 UI 포함)와 SQLite 파일 하나(`<데이터 디렉터리>/pango.db`, 첨부파일 포함)로 돕니다. 이 문서는 운영자를 위한 것입니다. Buzz 알림 설정은 [BUZZ.md](BUZZ.md)를 봅니다.

## 1. 실행 파일 만들기

### 직접 빌드

```bash
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version 0.2.130 --locked
cargo xtask web                              # 웹 UI 번들(실행 파일에 포함된다)
cargo build --release --locked -p pango-server
# 결과: target/release/pango-groupware
```

`cargo xtask web`을 빼먹으면 "웹 UI가 빌드되지 않았습니다"라는 안내 페이지만 나옵니다.

### Docker

저장소 루트의 `Dockerfile`로 이미지를 만듭니다. 이미지 안에서 데이터는 `/data` 볼륨에 있고 `0.0.0.0:8080`에서 받습니다. 프로세스는 root가 아닌 `pango`(uid 10001)로 돕니다.

```bash
docker build -t pango-groupware .
docker volume create pango-data
docker run --rm -it -v pango-data:/data pango-groupware init --company "우리 회사"   # 임시 비밀번호가 한 번 출력된다
docker run -d --name pango -p 127.0.0.1:8080:8080 -v pango-data:/data \
  -e PANGO_TRUST_PROXY=true -e PANGO_PUBLIC_ORIGIN=https://gw.example.com \
  pango-groupware
```

## 2. 최초 설정과 실행

```bash
pango-groupware --data-dir /var/lib/pango init --company "우리 회사"
pango-groupware --data-dir /var/lib/pango serve --bind 127.0.0.1:8080 --trust-proxy \
  --public-origin https://gw.example.com
```

| 옵션 | 환경 변수 | 설명 |
| --- | --- | --- |
| `--data-dir` | `PANGO_DATA_DIR` | 데이터 디렉터리(기본 `./data`) |
| `--bind` | `PANGO_BIND` | 받을 주소(기본 `127.0.0.1:8080`) |
| `--trust-proxy` | `PANGO_TRUST_PROXY` | 앞단 프록시의 `X-Forwarded-For`를 믿는다(프록시 뒤에서만) |
| `--public-origin` | `PANGO_PUBLIC_ORIGIN` | 공개 주소. 프록시 뒤에서는 지정을 권장(CSRF 출처 확인 기준) |
| `--insecure-cookies` | — | HTTPS 없는 로컬 시험용. 운영에서는 쓰지 않는다 |
| `--max-attachment-mb` | `PANGO_MAX_ATTACHMENT_MB` | 첨부파일 하나의 최대 크기(기본 10, 1~100) |
| `--mcp`, `--mcp-allow-loopback` | `PANGO_MCP`, `PANGO_MCP_ALLOW_LOOPBACK` | Claude 등 AI 앱 연결(원격 MCP). [MCP.md](MCP.md) |
| `--outbound-allow-private` | `PANGO_OUTBOUND_ALLOW_PRIVATE` | 웹훅·Buzz 릴레이를 사설망 주소로도 보낸다 |
| Buzz 설정 | `PANGO_BUZZ_RELAY_URL` 등 | [BUZZ.md](BUZZ.md) |

세션 쿠키에는 `Secure`가 붙으므로 **HTTPS로만** 접속해야 로그인이 유지됩니다.

## 3. 리버스 프록시(TLS)

앞단에서 TLS를 끝내고 `127.0.0.1:8080`으로 넘깁니다. Caddy 예시:

```
gw.example.com {
    reverse_proxy 127.0.0.1:8080
    request_body {
        max_size 12MB   # 첨부파일 상한(기본 10 MiB)보다 조금 크게
    }
}
```

nginx라면 `client_max_body_size 12m;`와 `proxy_set_header X-Forwarded-For $remote_addr;`를 넣습니다(서버는 `X-Forwarded-For`의 **마지막** 값만 믿습니다).

로드밸런서(AWS ALB 등)의 상태 확인 경로는 `GET /api/v1/health`입니다(SQLite 읽기 풀 연결까지 확인해 정상이면 200, 장애면 503). 정적 웹 파일(WASM·JS·CSS)은 클라이언트가 `Accept-Encoding: gzip`을 보내면 서버가 직접 압축해 응답합니다.

## 4. systemd로 돌리기

```ini
# /etc/systemd/system/pango-groupware.service
[Unit]
Description=pango-groupware
After=network-online.target
Wants=network-online.target

[Service]
User=pango
Group=pango
ExecStart=/usr/local/bin/pango-groupware --data-dir /var/lib/pango serve --bind 127.0.0.1:8080 --trust-proxy --public-origin https://gw.example.com
Restart=on-failure
NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=true
PrivateTmp=true
ReadWritePaths=/var/lib/pango /var/backups/pango
# Buzz 봇 키 등 비밀값은 권한을 600으로 둔 파일에서 읽는다.
# EnvironmentFile=/etc/pango-groupware.env

[Install]
WantedBy=multi-user.target
```

```bash
sudo useradd --system --home-dir /var/lib/pango --shell /usr/sbin/nologin pango
sudo install -d -o pango -g pango -m 700 /var/lib/pango /var/backups/pango
sudo systemctl enable --now pango-groupware
```

## 5. 백업과 복구

서버가 도는 중에도 일관된 사본을 만들 수 있습니다(`VACUUM INTO`, 쓰기를 막지 않음). 사본에는 개인정보와 첨부파일, 2단계 인증 비밀키가 모두 들어 있으므로 파일 권한은 600으로 만들어지고, 다른 곳에 보관할 때도 암호화해 둡니다.

```bash
pango-groupware --data-dir /var/lib/pango backup /var/backups/pango/pango-$(date +%F).db
```

**실행 중인 DB 파일(`pango.db`, `-wal`, `-shm`)을 그대로 복사하지 마세요.** 쓰기 도중의 파일은 깨진 사본이 됩니다.

매일 새벽에 만들고 30일 지난 사본을 지우는 예(`pango` 사용자의 crontab):

```
15 3 * * * /usr/local/bin/pango-groupware --data-dir /var/lib/pango backup /var/backups/pango/pango-$(date +\%F).db && find /var/backups/pango -name 'pango-*.db' -mtime +30 -delete
```

Docker라면 `docker exec pango pango-groupware backup /data/pango-$(date +%F).db`로 볼륨 안에 만든 뒤 꺼냅니다.

`backup`은 마이그레이션을 돌리지 않는 읽기 전용 연결로 사본을 만들므로, 판올림 전에 새 실행 파일로 백업해도 운영 중인 DB가 바뀌지 않습니다.

**복구**: 서버를 멈추고, 데이터 디렉터리의 `pango.db`를 사본으로 바꾸고 `pango.db-wal`·`pango.db-shm`을 지운 뒤 다시 실행합니다. 복구한 시점 이후의 변경은 사라집니다.

**최고 관리자 계정 복구**: 최고 관리자가 비밀번호나 2단계 인증 앱을 잃었고 다른 최고 관리자가 없으면 웹에서는 되살릴 수 없습니다. 서버에서 다음을 실행하면 임시 비밀번호를 한 번 보여 주고, 2단계 인증을 끄고, 그 계정의 모든 로그인을 끊습니다(서버가 도는 중에도 됩니다). 감사 로그에 `system.owner_recover`로 남습니다.

```bash
sudo -u pango pango-groupware --data-dir /var/lib/pango reset-owner admin@example.com
```

## 6. 판올림

새 실행 파일로 바꾸고 다시 실행하면 DB 마이그레이션이 시작할 때 자동으로 적용됩니다. 되돌릴 수 없으므로 **판올림 전에 백업**을 만듭니다. 새 기본 결재 양식도 시작할 때 채워집니다(이미 있는 양식은 그대로).
