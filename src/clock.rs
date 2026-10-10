//! The person's day: their time zone and the world's `day_starts` setting,
//! never the machine's clock. Before the day starts it is still the day
//! before, so a review at two in the morning belongs to the evening before.
//!
//! ```no_run
//! # async fn run(link: orbit::link::Link) {
//! use orbit::clock::Clock;
//!
//! let clock = Clock::of(&link).await;
//! let today = clock.today();
//! let began = clock.start_of(today);
//! # }
//! ```

use std::collections::BTreeMap;

use chrono::{DateTime, Duration, LocalResult, NaiveDate, NaiveTime, TimeZone, Utc};
use chrono_tz::Tz;

use crate::link::Link;

/// The setting that says when a world's day begins, as `HH:MM`.
pub const DAY_STARTS: &str = "day_starts";

/// Where a day is lived: a time zone, and the time of day it begins.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Clock {
    pub tz: Tz,
    /// When a day begins, e.g. 04:00: anything before counts for the day before.
    pub day_starts: NaiveTime,
}

impl Default for Clock {
    /// UTC, with days that begin at midnight.
    fn default() -> Self {
        Self::new(Tz::UTC, NaiveTime::MIN)
    }
}

impl Clock {
    pub fn new(tz: Tz, day_starts: NaiveTime) -> Self {
        Self { tz, day_starts }
    }

    /// The person's time zone and the world's `day_starts` setting, as Sol
    /// last sent them. Days begin at midnight when the world has no such
    /// setting or it isn't a time.
    pub async fn of(link: &Link) -> Self {
        let settings = link.settings().await.unwrap_or_default();
        Self::with(link, &settings)
    }

    /// The same, with the settings already read.
    pub fn with(link: &Link, settings: &BTreeMap<String, serde_json::Value>) -> Self {
        Self::new(
            link.time_zone(),
            time_setting(settings, DAY_STARTS).unwrap_or(NaiveTime::MIN),
        )
    }

    /// Now, on the person's wall clock.
    pub fn now(&self) -> DateTime<Tz> {
        Utc::now().with_timezone(&self.tz)
    }

    /// The day it is now.
    pub fn today(&self) -> NaiveDate {
        self.day_of(Utc::now())
    }

    /// The day a moment belongs to: its date in the person's zone, or the
    /// date before when it is earlier than the day's start.
    pub fn day_of(&self, at: DateTime<Utc>) -> NaiveDate {
        let local = at.with_timezone(&self.tz);
        if local.time() < self.day_starts {
            local.date_naive() - Duration::days(1)
        } else {
            local.date_naive()
        }
    }

    /// A time of day on a date, in the person's zone. When the clocks go
    /// back and it happens twice, the first; when they skip it (spring
    /// forward), the same time an hour later.
    pub fn at(&self, date: NaiveDate, time: NaiveTime) -> DateTime<Utc> {
        let local = date.and_time(time);
        match self.tz.from_local_datetime(&local) {
            LocalResult::Single(t) | LocalResult::Ambiguous(t, _) => t.with_timezone(&Utc),
            LocalResult::None => self
                .tz
                .from_local_datetime(&(local + Duration::hours(1)))
                .earliest()
                .map_or_else(|| local.and_utc(), |t| t.with_timezone(&Utc)),
        }
    }

    /// The moment a day begins; the next day's start is where it ends.
    pub fn start_of(&self, date: NaiveDate) -> DateTime<Utc> {
        self.at(date, self.day_starts)
    }
}

/// `HH:MM`, as time settings are written.
pub fn parse_time(s: &str) -> Option<NaiveTime> {
    NaiveTime::parse_from_str(s.trim(), "%H:%M").ok()
}

/// A time setting of the world's (see [`Link::settings`]), when it is set
/// and is a time.
pub fn time_setting(
    settings: &BTreeMap<String, serde_json::Value>,
    key: &str,
) -> Option<NaiveTime> {
    settings.get(key)?.as_str().and_then(parse_time)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn utc(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn date(s: &str) -> NaiveDate {
        s.parse().unwrap()
    }

    fn warsaw(starts: &str) -> Clock {
        Clock::new(
            "Europe/Warsaw".parse().unwrap(),
            parse_time(starts).unwrap(),
        )
    }

    #[test]
    fn early_hours_belong_to_the_day_before() {
        let clock = warsaw("04:00");
        // 02:00 and 04:00 in Warsaw.
        assert_eq!(
            clock.day_of(utc("2026-03-01T01:00:00Z")),
            date("2026-02-28")
        );
        assert_eq!(
            clock.day_of(utc("2026-03-01T03:00:00Z")),
            date("2026-03-01")
        );
        // At midnight, days are calendar days in the person's zone.
        let midnight = warsaw("00:00");
        assert_eq!(
            midnight.day_of(utc("2026-02-28T23:30:00Z")),
            date("2026-03-01")
        );
        assert_eq!(
            Clock::default().day_of(utc("2026-02-28T23:30:00Z")),
            date("2026-02-28")
        );
    }

    #[test]
    fn days_begin_across_the_clock_changes() {
        let clock = warsaw("04:00");
        assert_eq!(
            clock.start_of(date("2026-03-01")),
            utc("2026-03-01T03:00:00Z")
        );
        // Summer time: 04:00 is two hours ahead of UTC.
        assert_eq!(
            clock.start_of(date("2026-07-01")),
            utc("2026-07-01T02:00:00Z")
        );
        // 02:30 is skipped on 29 March: the day begins at 03:30 instead.
        let skipped = warsaw("02:30");
        assert_eq!(
            skipped.start_of(date("2026-03-29")),
            utc("2026-03-29T01:30:00Z")
        );
        // 02:30 happens twice on 25 October: the day begins at the first.
        assert_eq!(
            skipped.start_of(date("2026-10-25")),
            utc("2026-10-25T00:30:00Z")
        );
    }

    #[test]
    fn times_are_read_from_settings() {
        assert_eq!(parse_time(" 07:30 "), NaiveTime::from_hms_opt(7, 30, 0));
        assert_eq!(parse_time("7.30"), None);
        assert_eq!(parse_time("25:00"), None);
        let settings = BTreeMap::from([
            ("day_starts".to_owned(), json!("04:00")),
            ("evening".to_owned(), json!(20)),
        ]);
        assert_eq!(
            time_setting(&settings, DAY_STARTS),
            NaiveTime::from_hms_opt(4, 0, 0)
        );
        assert_eq!(time_setting(&settings, "evening"), None);
        assert_eq!(time_setting(&settings, "missing"), None);
    }

    #[tokio::test]
    async fn an_unpaired_link_lives_in_utc_from_midnight() {
        let dir = tempfile::tempdir().unwrap();
        let link = Link::open(crate::link::Config {
            world: "terra".into(),
            dir: dir.path().to_owned(),
            device: "Test".into(),
            platform: None,
            version: "0.1.0".into(),
            tokens: crate::link::Tokens::File,
        })
        .await
        .unwrap();
        assert_eq!(link.time_zone(), Tz::UTC);
        assert_eq!(Clock::of(&link).await, Clock::default());
    }
}
