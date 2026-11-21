//! 화면 경로. 경로를 추가할 때는 여기와 `layout.rs`의 메뉴만 고친다.

use dioxus::prelude::*;

use crate::layout::Shell;
use crate::pages::admin::{
    AdminApprovalForms, AdminAttendance, AdminAudit, AdminHome, AdminIntegrations, AdminLeave,
    AdminMembers, AdminNotices, AdminOrg, AdminPolicy,
};
use crate::pages::{
    ApprovalDetail, ApprovalList, ApprovalNew, Attendance, Calendar, CalendarEvent, ChangePassword,
    Home, Leave, LeaveNew, Login, More, NotFound, NoticeDetail, Notices, Notifications, People,
    Profile, Team,
};

#[derive(Debug, Clone, PartialEq, Routable)]
#[rustfmt::skip]
pub enum Route {
    #[route("/login")]
    Login {},
    #[route("/password")]
    ChangePassword {},

    #[layout(Shell)]
        #[route("/")]
        Home {},
        #[route("/attendance")]
        Attendance {},
        #[route("/leave")]
        Leave {},
        #[route("/leave/new")]
        LeaveNew {},
        #[route("/calendar")]
        Calendar {},
        /// 알림이 가리키는 일정(`calendar.invited` 등). 캘린더를 열고 그 일정을 띄운다.
        #[route("/calendar/events/:id")]
        CalendarEvent { id: String },
        #[route("/approval")]
        ApprovalList {},
        #[route("/approval/new")]
        ApprovalNew {},
        #[route("/approval/documents/:id")]
        ApprovalDetail { id: String },
        #[route("/notices")]
        Notices {},
        #[route("/notices/:id")]
        NoticeDetail { id: String },
        #[route("/notifications")]
        Notifications {},
        #[route("/people")]
        People {},
        #[route("/team")]
        Team {},
        #[route("/me")]
        Profile {},
        #[route("/more")]
        More {},
        #[route("/admin")]
        AdminHome {},
        #[route("/admin/members")]
        AdminMembers {},
        #[route("/admin/org")]
        AdminOrg {},
        #[route("/admin/attendance")]
        AdminAttendance {},
        #[route("/admin/leave")]
        AdminLeave {},
        #[route("/admin/forms")]
        AdminApprovalForms {},
        #[route("/admin/notices")]
        AdminNotices {},
        #[route("/admin/policy")]
        AdminPolicy {},
        #[route("/admin/integrations")]
        AdminIntegrations {},
        #[route("/admin/audit")]
        AdminAudit {},
    #[end_layout]

    #[route("/:..segments")]
    NotFound { segments: Vec<String> },
}
