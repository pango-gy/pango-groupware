# Buzz 연동 운영 가이드

그룹웨어 알림을 사내 메신저 **Buzz**(Nostr 릴레이, [block/buzz](https://github.com/block/buzz))의 DM으로도 보내는 방법입니다. 서버를 운영하는 사람을 위한 문서입니다. 구성원은 그룹웨어 **내 정보 → Buzz 연결**에서 직접 연결하거나(확인 코드), 관리자가 **관리 → 외부 알림 연동 → 구성원 Buzz ID 일괄 등록** 또는 **관리 → 구성원 → Buzz 연결**에서 대신 등록합니다.

관리자 등록은 본인 확인 없이 바로 연결됩니다. 잘못 넣은 ID를 받은 사람이 알 수 있도록 등록하면 구성원에게 "Buzz 알림이 연결됐어요" 알림(줄인 Buzz ID 포함)이 앱과 Buzz로 함께 갑니다. 구성원은 내 정보에서 언제든 연결을 끊을 수 있고, 등록은 감사 로그(`buzz.admin_link`)에 남습니다. 일괄 등록은 한 줄에 `아이디 Buzz ID`이며 줄마다 결과를 보여 주고, 문제가 있는 줄만 빼고 나머지를 등록합니다.

> 아래의 주소·키는 모두 **예시 값**입니다. 실제 값으로 바꿔 쓰고, 비밀키는 문서·채팅·이슈에 붙여 넣지 마세요.

## 어떻게 동작하나요

```text
그룹웨어 알림 생성 ──(같은 트랜잭션)──▶ 아웃박스(outbound_messages, channel='buzz')
                                              │ 5초마다 발송 작업
                                              ▼
              POST https://buzz.example.com/events   (NIP-98 인증, 봇 키로 서명)
                ├─ kind 41010  DM 열기(처음 한 번, 채널 ID를 저장해 다시 씀)
                └─ kind 9      메시지: 제목 / 짧은 본문 / 링크
```

- 그룹웨어는 **알림 봇 계정** 하나로 보냅니다. Buzz 사용자는 32바이트 공개키(`npub1…`)로 구분되고, REST 토큰·봇 토큰은 없습니다.
- 요청마다 NIP-98 머리글(`Authorization: Nostr <base64(서명한 kind 27235 이벤트)>`)을 붙입니다. 태그는 `u`(= `{릴레이 주소}/events`), `method`(`POST`), `payload`(본문 SHA-256)입니다. 릴레이는 서명 시각이 **±120초** 안이어야 받으므로 그룹웨어 서버의 시계를 NTP로 맞춰 두세요.
- Buzz는 이메일로 사용자를 찾을 수 없고 NIP-05도 자동으로 붙지 않습니다. 그래서 **구성원이 자기 Buzz ID를 직접 입력**하고, 봇이 DM으로 보낸 6자리 코드를 다시 입력해 소유를 증명합니다(10분 유효, 5번 틀리면 무효, 60초 간격·시간당 5번까지).
- kind 9 DM은 **릴레이에 평문으로 저장**됩니다. 그래서 메시지에는 앱 알림에 이미 보이는 제목·짧은 본문·그룹웨어 링크만 넣고, 휴가 사유나 결재 문서 내용은 넣지 않습니다.
- 재시도: 429·5xx·연결 실패는 30초부터 두 배씩 최대 8번, 401·403(봇이 구성원이 아님·서명 오류)과 그 밖의 4xx는 바로 실패입니다. 관리자 화면의 **발송 현황**에서 최근 실패 사유를 볼 수 있습니다.

## 1. 봇 키 만들기

릴레이 이미지 안의 `buzz-admin`으로 키 쌍을 만듭니다. 공개키·비밀키 모두 **16진수 64자**로 나옵니다.

```bash
# docker compose로 띄운 경우(deploy/compose)
docker compose exec relay /usr/local/bin/buzz-admin generate-key
# Public key:  3bf0c63f…(64자)
# Secret key:  9f1c…(64자)    ← 비밀키. 화면 기록이 남지 않는 곳에서 실행하세요.
```

## 2. 비밀키를 AWS Secrets Manager에 넣기

```bash
aws secretsmanager create-secret \
  --name groupware/buzz-bot-key \
  --secret-string '<위의 Secret key 64자>'
```

그룹웨어가 도는 곳(ECS 작업, EC2, Kubernetes)에서 이 비밀을 **파일로 마운트**하거나 환경 변수로 주입합니다. 파일 마운트를 권장합니다(프로세스 환경 목록에 남지 않습니다).

## 3. 그룹웨어 실행 설정

| 설정 | 예 | 설명 |
| --- | --- | --- |
| `PANGO_BUZZ_RELAY_URL` / `--buzz-relay-url` | `https://buzz.example.com` | 릴레이 주소. `wss://`로 적어도 됩니다(같은 호스트의 HTTPS API를 씀). 경로 없이 출처만 |
| `PANGO_BUZZ_BOT_KEY_FILE` / `--buzz-bot-key-file` | `/run/secrets/buzz-bot-key` | 비밀키 파일 경로(권장). 내용은 `nsec1…` 또는 16진수 64자 |
| `PANGO_BUZZ_BOT_KEY` | (환경 변수로만) | 비밀키 값. 명령줄 인자로는 받지 않습니다. `_FILE`과 함께 주면 시작하지 않습니다 |
| `PANGO_OUTBOUND_ALLOW_PRIVATE` / `--outbound-allow-private` | `true` | 릴레이가 **같은 VPC의 사설 IP**(10/8, 172.16/12, 192.168/16, 100.64/10, fc00::/7)나 루프백에 있을 때만 켭니다 |

- 주소와 키는 함께 있어야 하고, 하나만 있으면 서버가 시작하지 않습니다(오류 문구에 키 값은 나오지 않습니다).
- 비밀키는 DB·관리자 화면·감사 로그·로그 어디에도 남지 않습니다. 화면에는 봇의 **공개키(npub)** 만 보입니다.
- 클라우드 메타데이터(`169.254.169.254`, `fd00:ec2::254`)·link-local 주소는 `--outbound-allow-private`를 켜도 막힙니다. 시스템 프록시(`HTTP_PROXY`)는 쓰지 않습니다.

```bash
PANGO_BUZZ_RELAY_URL=https://buzz.example.com \
PANGO_BUZZ_BOT_KEY_FILE=/run/secrets/buzz-bot-key \
pango-groupware --data-dir /var/lib/pango serve --bind 0.0.0.0:8080 \
  --public-origin https://gw.example.com
```

`--public-origin`을 주면 메시지의 링크가 `https://gw.example.com/approval/documents/…` 같은 절대 주소가 됩니다(없으면 앱 안의 경로만 갑니다).

시작 로그에 `Buzz channel configured relay=… bot=npub1…`이 나오면 설정이 읽힌 것입니다.

## 4. 봇을 릴레이 구성원으로 등록

`BUZZ_REQUIRE_RELAY_MEMBERSHIP=true`인 릴레이는 구성원이 아닌 계정의 쓰기를 거절합니다(그룹웨어 화면에는 "알림 봇을 받아 주지 않았어요"로 보입니다). 1단계의 **공개키**(또는 관리자 화면의 봇 npub)를 등록합니다.

```bash
# docker compose(deploy/compose/run.sh)
./run.sh add-member <봇 공개키 hex 또는 npub> --role member

# Helm(Kubernetes) — 확인 필요: Deployment 이름은 차트의 `buzz.fullname`(보통 릴리스 이름)이라
# 배포마다 다릅니다. `kubectl get deploy -n <네임스페이스>`로 확인하세요.
kubectl exec -n buzz deploy/<릴리스 이름> -- \
  /usr/local/bin/buzz-admin add-member --pubkey <봇 공개키> --role member
```

`buzz-admin add-member`는 릴레이의 `BUZZ_RELAY_PRIVATE_KEY`가 필요합니다(릴레이 컨테이너 안에서는 이미 설정돼 있습니다).

## 5. 봇 프로필 이름 정하기

Buzz에서 DM을 받는 사람이 누가 보냈는지 알 수 있도록 이름을 붙입니다.

```bash
BUZZ_RELAY_URL=https://buzz.example.com \
BUZZ_PRIVATE_KEY="$(cat /run/secrets/buzz-bot-key)" \
buzz users set-profile --name "결재 알림"
```

## 6. Buzz 보안 설정 확인

| 릴레이 설정 | 권장 | 이유 |
| --- | --- | --- |
| `BUZZ_REQUIRE_AUTH_TOKEN` | `true` (**필수**) | 기본값은 `false`이고, 이때 REST 요청은 서명 없는 `X-Pubkey` 개발용 경로로도 통과합니다. 그러면 누구나 봇인 척 쓸 수 있습니다. 켜야 NIP-98 서명이 강제됩니다 |
| `BUZZ_REQUIRE_RELAY_MEMBERSHIP` | `true` | 회사 구성원(과 등록한 봇)만 쓰게 합니다 |
| `BUZZ_RATE_LIMIT_HUMAN_API_CALLS_PER_MIN` | 기본 300 | 봇도 계정당 이 한도를 씁니다. 알림 한 건 = 요청 1번(처음 한 번은 DM 열기까지 2번). 큰 공지를 한꺼번에 보내면 429가 날 수 있고, 그룹웨어는 나중에 다시 보냅니다. 구성원이 많으면 늘려 주세요 |

릴레이 자체의 주소 설정(`RELAY_URL`)이 `wss://`여야 NIP-98의 `u` 태그를 `https://…/events`로 비교합니다. TLS를 앞단 프록시에서 끝내도 릴레이 설정은 `wss://`로 두세요(확인 필요: 배포 방식에 따라 다를 수 있습니다).

## 7. 켜기와 연결 확인

1. 그룹웨어 **관리 → 외부 알림 연동 → Buzz**에서 "서버 설정: 설정됨", 봇 ID가 보이는지 확인하고 **Buzz로 보내기 켜기**를 누릅니다.
2. 본인이 **내 정보 → Buzz 연결**에서 Buzz ID를 넣고 코드를 받아 봅니다. 구성원이 많으면 관리자가 **구성원 Buzz ID 일괄 등록**으로 한꺼번에 넣습니다.
3. 결재 요청 같은 알림을 하나 만들어 Buzz DM으로 오는지 봅니다.

## 문제 해결

| 증상(화면·발송 현황) | 원인 | 할 일 |
| --- | --- | --- |
| "서버 설정 필요" | 실행 설정 없음 | 3단계. 시작 로그 확인 |
| `http_401` / "알림 봇을 받아 주지 않았어요" | 서명 시각 차이(±120초), `u` 태그 불일치(릴레이 주소·스킴), 키 오류 | 서버 시계(NTP), `PANGO_BUZZ_RELAY_URL`이 구성원이 접속하는 주소와 같은지, 릴레이의 `RELAY_URL`이 `wss://`인지 |
| `http_403` / "알림 봇을 받아 주지 않았어요" | 봇이 릴레이 구성원이 아님 | 4단계 |
| "이 ID로 보내는 메시지를 거절했어요" | 받는 사람이 릴레이 구성원이 아니거나 ID를 잘못 넣음 | 구성원이 회사 Buzz에서 복사한 ID인지 확인 |
| `blocked_address` / "Buzz에 연결하지 못했어요" | 릴레이가 사설 IP인데 옵션이 꺼짐 | 같은 VPC면 `--outbound-allow-private` |
| `connect_failed`·`timeout` | 네트워크·보안 그룹 | 그룹웨어 → 릴레이 443 허용 |
| `http_429` | 요청 한도 | 자동 재시도. 잦으면 6단계의 한도 조정 |
| `recipient_unlinked` | 보내기 전에 구성원이 연결을 끊음 | 정상 |

## 한 사람이 여러 기기에서 쓰기

Buzz는 기기에 저장된 키가 곧 계정입니다. 모바일 앱에서 커뮤니티 주소를 넣은 뒤 **"Create an identity and join"**을 누르면 새 키가 만들어져 데스크톱과 **다른 계정**이 됩니다(그룹웨어에는 한 계정만 연결되므로 다른 기기에는 알림이 가지 않습니다).

- 먼저 데스크톱 **Settings… → Identity → Community sign-ins**에서 로그인 방식을 확인합니다.
  - **Builderlab(회사 이메일) 계정**이면 Pair mobile 화면이 없고, **Settings… → Mobile app**에 "휴대폰에서 Builderlab에 같은 이메일로 로그인하라"는 안내가 나옵니다. 모바일에서 Sign out 후 같은 이메일로 Builderlab 로그인을 합니다. 커뮤니티 초대 QR을 찍으면 새 계정이 생깁니다(실제로 확인한 사례).
  - 키로만 쓰는 계정이면: 데스크톱 **Settings… → Pair mobile**에서 커뮤니티 주소를 넣고 QR을 띄운 뒤, 모바일 **Add community → Scan a QR code**(또는 **Use a pairing code**)로 연결합니다. 키는 종단 간 암호화로 전달되고, 두 기기에 뜬 확인 코드를 비교합니다(근거: buzz-app `src/bundled/pairing/PairingSettings.tsx`, buzz `mobile/lib/features/pairing/`).
- 이미 두 번째 계정이 생겼으면:
  1. 모바일 **Settings → Sign out**으로 나간 뒤 위 방법으로 다시 연결합니다(키는 기기의 저장된 신원 목록에 남을 수 있습니다).
  2. 릴레이에서 그 키를 구성원에서 뺍니다. 관리자는 데스크톱 **Settings… → Membership**에서 그 계정의 ⋯ → Remove로 뺄 수 있습니다(검색은 npub 전체가 아니라 이름이나 hex 공개키 앞부분으로 됩니다). 서버에서는 `./run.sh remove-member <npub>`(직접 실행: `buzz-admin remove-member --pubkey <npub|hex>`). 그 키로 쓴 메시지·프로필이 남는지는 확인하지 못했습니다.
- 예방:
  - `BUZZ_REQUIRE_RELAY_MEMBERSHIP=true`(코드 기본값은 `false`라 아무 새 키나 쓸 수 있습니다).
  - 초대 링크는 기본으로 **사용 횟수 무제한·72시간** 유효이고, 받은 새 키를 바로 구성원으로 넣습니다. 초대는 `POST /api/invites` `{"max_uses":1,"ttl_secs":…}`로 1회용·단기 발급하고, 남은 초대는 `/api/invites/revoke`로 회수합니다. 이미 계정이 있는 사람에게는 초대 대신 Pair mobile을 안내합니다.

## 모바일 푸시에 대해

Buzz 모바일 앱의 푸시 알림은 Buzz 쪽 **실험 기능**이고, 별도의 `buzz-push-gateway`와 APNs/FCM 설정을 따로 배포해야 합니다. 그룹웨어는 DM을 보낼 뿐이므로, 푸시가 오는지는 회사 Buzz의 푸시 설정에 달려 있습니다. 또 봇과의 **새 DM에 온 첫 메시지**는 앱이 그 DM을 아직 모르는 상태라 푸시가 늦거나 오지 않을 수 있습니다(추론: 소스 기준으로 확인하지 못했습니다). 연결할 때 확인 코드 DM이 이미 한 번 오가므로 보통 알림부터는 같은 DM으로 갑니다.
