//! 메뉴에 붙는 읽지 않은 수(알림·공지). 셸이 한 번 제공하고, 읽음 상태를 바꾼 화면이
//! [`refresh_badges`]로 다시 읽게 한다.

use dioxus::prelude::*;
use pango_api::notice::NoticeUnreadCount;
use pango_api::notification::UnreadCount;

use crate::api;
use crate::route::Route;

#[derive(Clone, Copy)]
pub struct Badges {
    notifications: Signal<u32>,
    notices: Signal<u32>,
}

impl Badges {
    pub fn notifications(&self) -> u32 {
        *self.notifications.read()
    }

    pub fn notices(&self) -> u32 {
        *self.notices.read()
    }

    /// 메뉴 항목에 붙일 수. 배지가 없는 메뉴는 0.
    pub fn for_route(&self, route: &Route) -> u32 {
        match route {
            Route::Notices {} => self.notices(),
            Route::Notifications {} => self.notifications(),
            _ => 0,
        }
    }

    pub async fn refresh(mut self) {
        // 배지는 보조 정보라 실패하면 이전 값을 둔다. 세션 만료는 화면의 본 요청이 처리한다.
        if let Ok(count) = api::get::<UnreadCount>("/notifications/unread-count").await {
            self.notifications.set(count.unread);
        }
        if let Ok(count) = api::get::<NoticeUnreadCount>("/notices/unread-count").await {
            self.notices.set(count.unread);
        }
    }
}

pub fn provide_badges() -> Badges {
    use_context_provider(|| Badges {
        notifications: Signal::new(0),
        notices: Signal::new(0),
    })
}

pub fn use_badges() -> Badges {
    use_context::<Badges>()
}

/// 배지를 다시 읽는다. 훅이 아니므로 이벤트 핸들러 안에서 부른다.
/// 화면을 떠나도 끝까지 돌도록 앱 수명의 작업으로 띄운다.
pub fn refresh_badges() {
    if let Some(badges) = try_consume_context::<Badges>() {
        dioxus::dioxus_core::spawn_forever(badges.refresh());
    }
}

/// 배지 숫자. 너무 크면 `99+`.
pub fn count_text(count: u32) -> String {
    if count > 99 {
        "99+".to_owned()
    } else {
        count.to_string()
    }
}

/// 숫자만 보고는 뜻을 알 수 없으므로 화면 낭독기용 이름에 읽지 않은 수를 붙인다.
pub fn unread_label(label: &str, count: u32) -> String {
    if count == 0 {
        label.to_owned()
    } else {
        format!("{label}, 읽지 않음 {}개", count_text(count))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_are_capped_for_display() {
        assert_eq!(count_text(0), "0");
        assert_eq!(count_text(99), "99");
        assert_eq!(count_text(100), "99+");
        assert_eq!(count_text(u32::MAX), "99+");
    }

    #[test]
    fn unread_label_names_the_count() {
        assert_eq!(unread_label("알림", 0), "알림");
        assert_eq!(unread_label("알림", 3), "알림, 읽지 않음 3개");
        assert_eq!(unread_label("알림", 120), "알림, 읽지 않음 99+개");
    }
}
