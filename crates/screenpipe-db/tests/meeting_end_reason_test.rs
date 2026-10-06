//! Regression tests for the meeting-merge bug fix.
//!
//! Background: when a user explicitly stopped a meeting and joined another
//! call shortly after, the auto-detector was reopening the just-stopped
//! meeting (because `find_recent_meeting_for_app` only checked for any
//! ended row within 120s). The live note then showed the previous call's
//! transcript tail, and the sync pipe emitted "DUPLICATE: X" notifications
//! because both meetings had been jammed into the same row.
//!
//! Fix: tag the row with `end_reason = 'explicit_stop'` on the stop API
//! path and exclude those rows from the merge query. These tests pin both
//! halves of that contract.

#[cfg(test)]
mod tests {
    use screenpipe_db::{
        DatabaseManager, MEETING_END_REASON_AUTO_END, MEETING_END_REASON_EXPLICIT_STOP,
        MEETING_END_REASON_SHUTDOWN,
    };

    async fn setup_test_db() -> DatabaseManager {
        let db = DatabaseManager::new("sqlite::memory:", Default::default())
            .await
            .unwrap();

        sqlx::migrate!("./src/migrations")
            .run(&db.pool)
            .await
            .unwrap();

        db
    }

    async fn read_end_reason(db: &DatabaseManager, id: i64) -> Option<String> {
        let row: (Option<String>,) =
            sqlx::query_as("SELECT end_reason FROM meetings WHERE id = ?1")
                .bind(id)
                .fetch_one(&db.pool)
                .await
                .unwrap();
        row.0
    }

    async fn now_string() -> String {
        chrono::Utc::now()
            .format("%Y-%m-%dT%H:%M:%S%.3fZ")
            .to_string()
    }

    #[tokio::test]
    async fn end_meeting_persists_explicit_stop_reason() {
        let db = setup_test_db().await;
        let id = db.insert_meeting("Arc", "auto", None, None).await.unwrap();

        db.end_meeting(
            id,
            &now_string().await,
            Some(MEETING_END_REASON_EXPLICIT_STOP),
        )
        .await
        .unwrap();

        assert_eq!(
            read_end_reason(&db, id).await.as_deref(),
            Some(MEETING_END_REASON_EXPLICIT_STOP)
        );
    }

    #[tokio::test]
    async fn end_meeting_persists_none_for_natural_grace_timeout() {
        let db = setup_test_db().await;
        let id = db.insert_meeting("Arc", "auto", None, None).await.unwrap();

        db.end_meeting(id, &now_string().await, None).await.unwrap();

        assert_eq!(read_end_reason(&db, id).await, None);
    }

    #[tokio::test]
    async fn end_meeting_with_typed_text_persists_reason() {
        let db = setup_test_db().await;
        let id = db
            .insert_meeting("zoom.us", "auto", None, None)
            .await
            .unwrap();

        db.end_meeting_with_typed_text(
            id,
            &now_string().await,
            false,
            Some(MEETING_END_REASON_EXPLICIT_STOP),
        )
        .await
        .unwrap();

        assert_eq!(
            read_end_reason(&db, id).await.as_deref(),
            Some(MEETING_END_REASON_EXPLICIT_STOP)
        );
    }

    #[tokio::test]
    async fn reopen_meeting_clears_end_reason() {
        let db = setup_test_db().await;
        let id = db.insert_meeting("Arc", "auto", None, None).await.unwrap();

        db.end_meeting(
            id,
            &now_string().await,
            Some(MEETING_END_REASON_EXPLICIT_STOP),
        )
        .await
        .unwrap();
        assert_eq!(
            read_end_reason(&db, id).await.as_deref(),
            Some(MEETING_END_REASON_EXPLICIT_STOP)
        );

        db.reopen_meeting(id).await.unwrap();
        assert_eq!(read_end_reason(&db, id).await, None);
    }

    /// Regression: this is the load-bearing assertion for the meeting-merge bug.
    /// When the previous Arc meeting ended via explicit user stop, a new Arc
    /// meeting detected within the 120s window MUST NOT find it as a merge
    /// candidate.
    #[tokio::test]
    async fn find_recent_meeting_skips_explicit_stop() {
        let db = setup_test_db().await;
        let id = db.insert_meeting("Arc", "auto", None, None).await.unwrap();

        db.end_meeting(
            id,
            &now_string().await,
            Some(MEETING_END_REASON_EXPLICIT_STOP),
        )
        .await
        .unwrap();

        let found = db.find_recent_meeting_for_app("Arc", 120).await.unwrap();
        assert!(
            found.is_none(),
            "explicit_stop meeting must not be returned as a merge candidate, got {:?}",
            found.map(|m| m.id)
        );
    }

    /// Counter-regression: a naturally-ended row (end_reason NULL) within
    /// the window SHOULD still be returned — that's the legitimate merge
    /// case where the user dropped briefly and rejoined.
    #[tokio::test]
    async fn find_recent_meeting_returns_natural_end() {
        let db = setup_test_db().await;
        let id = db.insert_meeting("Arc", "auto", None, None).await.unwrap();

        db.end_meeting(id, &now_string().await, None).await.unwrap();

        let found = db.find_recent_meeting_for_app("Arc", 120).await.unwrap();
        assert_eq!(found.map(|m| m.id), Some(id));
    }

    /// Counter-regression: rows tagged with auto_end (server-side inactivity
    /// finalize) or shutdown (app quit ended the row) are still eligible
    /// for merge — those aren't user-initiated stops.
    #[tokio::test]
    async fn find_recent_meeting_returns_auto_end_and_shutdown() {
        let db = setup_test_db().await;

        let auto_id = db.insert_meeting("Arc", "auto", None, None).await.unwrap();
        db.end_meeting(
            auto_id,
            &now_string().await,
            Some(MEETING_END_REASON_AUTO_END),
        )
        .await
        .unwrap();
        let found = db.find_recent_meeting_for_app("Arc", 120).await.unwrap();
        assert_eq!(found.map(|m| m.id), Some(auto_id));

        // Tear down the auto_end row so it doesn't interfere
        sqlx::query("DELETE FROM meetings WHERE id = ?1")
            .bind(auto_id)
            .execute(&db.pool)
            .await
            .unwrap();

        let shut_id = db.insert_meeting("Arc", "auto", None, None).await.unwrap();
        db.end_meeting(
            shut_id,
            &now_string().await,
            Some(MEETING_END_REASON_SHUTDOWN),
        )
        .await
        .unwrap();
        let found = db.find_recent_meeting_for_app("Arc", 120).await.unwrap();
        assert_eq!(found.map(|m| m.id), Some(shut_id));
    }

    /// End-to-end reproduction of the user-reported sequence: meeting A stops
    /// via explicit user action, then a new meeting B is detected on the same
    /// app inside the merge window. B must NOT be merged into A.
    #[tokio::test]
    async fn explicit_stop_then_new_meeting_does_not_merge() {
        let db = setup_test_db().await;

        // Meeting A — user joins a Google Meet in Arc, talks, hits stop.
        let a = db
            .insert_meeting("Arc", "auto", Some("call A"), None)
            .await
            .unwrap();
        db.end_meeting_with_typed_text(
            a,
            &now_string().await,
            false,
            Some(MEETING_END_REASON_EXPLICIT_STOP),
        )
        .await
        .unwrap();

        // Detector immediately sees a new Arc meeting (the next call).
        // It calls find_recent_meeting_for_app — must return None so the
        // caller falls through to inserting a fresh row.
        let candidate = db.find_recent_meeting_for_app("Arc", 120).await.unwrap();
        assert!(candidate.is_none(), "must not merge into explicit-stop row");

        // Caller falls through to insert_meeting → fresh id.
        let b = db
            .insert_meeting("Arc", "auto", Some("call B"), None)
            .await
            .unwrap();
        assert_ne!(a, b, "new meeting must get its own id");
    }

    /// `close_orphaned_meetings` runs on app startup over leftover
    /// `meeting_end IS NULL` rows. It must tag them with `auto_end` so they
    /// stay eligible for merge but distinguishable from user-stopped rows.
    #[tokio::test]
    async fn close_orphaned_meetings_tags_auto_end() {
        let db = setup_test_db().await;

        // Insert an old auto-detected meeting with no end (simulates a row
        // left dangling from a crashed previous session).
        let id = sqlx::query(
            "INSERT INTO meetings (meeting_start, meeting_app, detection_source) \
             VALUES (?1, 'Arc', 'auto')",
        )
        .bind("2026-05-26T10:00:00Z")
        .execute(&db.pool)
        .await
        .unwrap()
        .last_insert_rowid();

        let closed = db.close_orphaned_meetings().await.unwrap();
        assert!(closed >= 1);
        assert_eq!(
            read_end_reason(&db, id).await.as_deref(),
            Some(MEETING_END_REASON_AUTO_END)
        );
    }

    // ---- orphan close must not make an old meeting look just-ended ----
    //
    // Issue #3: the orphan cleanup stamped `meeting_end = now`, so a row left
    // open days earlier fell inside the 120 s merge window, and the next call
    // on the same app was folded into it (a 71-hour, then a 4-day meeting).

    async fn insert_open_meeting(db: &DatabaseManager, app: &str, start: &str) -> i64 {
        sqlx::query(
            "INSERT INTO meetings (meeting_start, meeting_app, detection_source) \
             VALUES (?1, ?2, 'ui_scan')",
        )
        .bind(start)
        .bind(app)
        .execute(&db.pool)
        .await
        .unwrap()
        .last_insert_rowid()
    }

    async fn add_segment(db: &DatabaseManager, meeting_id: i64, at: chrono::DateTime<chrono::Utc>) {
        db.insert_meeting_transcript_segment(
            meeting_id,
            "selected-engine",
            None,
            &format!("item-{}", at.timestamp_millis()),
            "System Audio",
            "output",
            None,
            "synthetic segment",
            at,
        )
        .await
        .unwrap();
    }

    async fn read_end(db: &DatabaseManager, id: i64) -> String {
        let (end,): (String,) = sqlx::query_as("SELECT meeting_end FROM meetings WHERE id = ?1")
            .bind(id)
            .fetch_one(&db.pool)
            .await
            .unwrap();
        end
    }

    fn fmt(t: chrono::DateTime<chrono::Utc>) -> String {
        t.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()
    }

    #[tokio::test]
    async fn orphan_abandoned_days_ago_is_not_reopened_by_the_next_call() {
        let db = setup_test_db().await;
        let start = chrono::Utc::now() - chrono::Duration::days(4);
        let last = start + chrono::Duration::minutes(20);
        let id = insert_open_meeting(&db, "Zoom", &fmt(start)).await;
        add_segment(&db, id, start + chrono::Duration::minutes(1)).await;
        add_segment(&db, id, last).await;

        db.close_orphaned_meetings().await.unwrap();

        // SQLite's strftime goes through a floating-point julian day, so
        // the stamp can differ from the segment by a millisecond.
        let end = chrono::DateTime::parse_from_rfc3339(&read_end(&db, id).await).unwrap();
        let drift = (end.with_timezone(&chrono::Utc) - last)
            .num_milliseconds()
            .abs();
        assert!(drift <= 1, "ends at its last segment, off by {drift} ms");
        let candidate = db.find_recent_meeting_for_app("Zoom", 120).await.unwrap();
        assert!(
            candidate.is_none(),
            "a call starting now must get its own row"
        );
    }

    #[tokio::test]
    async fn orphan_from_a_crash_mid_call_stays_merge_eligible() {
        let db = setup_test_db().await;
        let start = chrono::Utc::now() - chrono::Duration::minutes(10);
        let id = insert_open_meeting(&db, "Zoom", &fmt(start)).await;
        add_segment(&db, id, chrono::Utc::now() - chrono::Duration::seconds(20)).await;

        db.close_orphaned_meetings().await.unwrap();

        let candidate = db.find_recent_meeting_for_app("Zoom", 120).await.unwrap();
        assert_eq!(candidate.map(|m| m.id), Some(id));
    }

    #[tokio::test]
    async fn orphan_with_no_segments_ends_at_its_start() {
        let db = setup_test_db().await;
        let start = fmt(chrono::Utc::now() - chrono::Duration::days(3));
        let id = insert_open_meeting(&db, "Zoom", &start).await;

        db.close_orphaned_meetings().await.unwrap();

        assert_eq!(read_end(&db, id).await, start);
        assert!(db
            .find_recent_meeting_for_app("Zoom", 120)
            .await
            .unwrap()
            .is_none());
    }
}
