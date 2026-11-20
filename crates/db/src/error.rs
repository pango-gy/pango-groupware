use pango_core::DomainError;

pub type DbResult<T> = Result<T, DbError>;

#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error("database error: {0}")]
    Sqlx(#[from] sqlx::Error),
    #[error("migration error: {0}")]
    Migrate(#[from] sqlx::migrate::MigrateError),
    /// 저장된 값이 도메인 규칙에 맞지 않는다. 버그이거나 DB가 외부에서 수정된 것이다.
    #[error("corrupt data in `{column}`: {source}")]
    Corrupt { column: &'static str, source: DomainError },
    #[error("corrupt json in `{column}`: {source}")]
    CorruptJson { column: &'static str, source: serde_json::Error },
}

impl DbError {
    /// 지정한 유일 제약(`table.column` 또는 인덱스 이름) 위반인지.
    ///
    /// SQLite는 제약 이름 대신 `UNIQUE constraint failed: members.login_id` 같은
    /// 메시지를 주므로 메시지에서 찾는다.
    pub fn is_unique_violation(&self, constraint: &str) -> bool {
        match self {
            Self::Sqlx(sqlx::Error::Database(db)) => {
                db.is_unique_violation() && db.message().contains(constraint)
            }
            _ => false,
        }
    }

    pub fn is_any_unique_violation(&self) -> bool {
        matches!(self, Self::Sqlx(sqlx::Error::Database(db)) if db.is_unique_violation())
    }

    pub fn is_foreign_key_violation(&self) -> bool {
        matches!(self, Self::Sqlx(sqlx::Error::Database(db)) if db.is_foreign_key_violation())
    }
}
