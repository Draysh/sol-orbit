//! The person's day: their time zone and the world's `day_starts` setting,
//! never the machine's clock. Before the day starts it is still the day
//! before, so a review at two in the morning belongs to the evening before.
//! Until the person sets `day_starts`, a day begins at [`DEFAULT_DAY_STARTS`]
//! (04:00), the default every world's manifest gives it.
//!
//! A world without a `day_starts` setting (Saturn, Neptune and their moons)
//! lives by calendar days instead: [`Clock::calendar`], whose days begin at
//! midnight in the person's zone. Either way "today" is never the machine's
//! own zone or UTC's date: every date that means the person's day goes
//! through a `Clock`.
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

/// When a day begins until the person sets `day_starts`: 04:00, the default
/// every manifest with the setting writes (each world's manifest test checks
/// it), so a tick at two in the morning counts for the day before.
pub const DEFAULT_DAY_STARTS: NaiveTime = match NaiveTime::from_hms_opt(4, 0, 0) {
    Some(t) => t,
    None => unreachable!(),
};

/// Where a day is lived: a time zone, and the time of day it begins.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Clock {
    pub tz: Tz,
    /// When a day begins, e.g. 04:00: anything before counts for the day before.
    pub day_starts: NaiveTime,
}

impl Default for Clock {
    /// UTC, with days that begin at [`DEFAULT_DAY_STARTS`].
    fn default() -> Self {
        Self::new(Tz::UTC, DEFAULT_DAY_STARTS)
    }
}

impl Clock {
    pub fn new(tz: Tz, day_starts: NaiveTime) -> Self {
        Self { tz, day_starts }
    }

    /// The person's time zone and the world's `day_starts` setting, as Sol
    /// last sent them. Days begin at [`DEFAULT_DAY_STARTS`] when the setting
    /// is unset or isn't a time, and when the settings can't be read (that
    /// is logged).
    pub async fn of(link: &Link) -> Self {
        let settings = link.settings().await.unwrap_or_else(|e| {
            tracing::warn!("couldn't read the settings for the day's start: {e:#}");
            BTreeMap::new()
        });
        Self::with(link, &settings)
    }

    /// For a world without a `day_starts` setting: the person's time zone,
    /// with days that begin at midnight.
    pub fn calendar(link: &Link) -> Self {
        Self::midnight(link.time_zone())
    }

    /// Calendar days in `tz`: each begins at midnight.
    pub fn midnight(tz: Tz) -> Self {
        Self::new(tz, NaiveTime::MIN)
    }

    /// The same, with the settings already read.
    pub fn with(link: &Link, settings: &BTreeMap<String, serde_json::Value>) -> Self {
        Self::from_settings(link.time_zone(), settings)
    }

    /// A time zone and the `day_starts` in `settings`, or
    /// [`DEFAULT_DAY_STARTS`] when it is unset or isn't a time.
    pub fn from_settings(tz: Tz, settings: &BTreeMap<String, serde_json::Value>) -> Self {
        Self::new(
            tz,
            time_setting(settings, DAY_STARTS).unwrap_or(DEFAULT_DAY_STARTS),
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
    fn an_unset_day_starts_at_four() {
        let tz: Tz = "Europe/Warsaw".parse().unwrap();
        let unset = Clock::from_settings(tz, &BTreeMap::new());
        assert_eq!(unset.day_starts, DEFAULT_DAY_STARTS);
        assert_eq!(DEFAULT_DAY_STARTS, parse_time("04:00").unwrap());
        // 02:00 in Warsaw is still the day before; 04:00 is the new day.
        assert_eq!(
            unset.day_of(utc("2026-03-01T01:00:00Z")),
            date("2026-02-28")
        );
        assert_eq!(
            unset.day_of(utc("2026-03-01T03:00:00Z")),
            date("2026-03-01")
        );
        // A setting that isn't a time counts as unset; midnight is honoured.
        let bad = BTreeMap::from([(DAY_STARTS.to_owned(), json!("soon"))]);
        assert_eq!(
            Clock::from_settings(tz, &bad).day_starts,
            DEFAULT_DAY_STARTS
        );
        let midnight = BTreeMap::from([(DAY_STARTS.to_owned(), json!("00:00"))]);
        assert_eq!(
            Clock::from_settings(tz, &midnight).day_starts,
            NaiveTime::MIN
        );
    }

    #[test]
    fn calendar_days_begin_at_midnight_in_the_persons_zone() {
        let clock = Clock::midnight("Europe/Warsaw".parse().unwrap());
        // 23:30 UTC is already the next day in Warsaw, and 00:30 there is not the day before.
        assert_eq!(
            clock.day_of(utc("2026-10-09T23:30:00Z")),
            date("2026-10-10")
        );
        assert_eq!(
            clock.day_of(utc("2026-10-09T21:59:00Z")),
            date("2026-10-09")
        );
        assert_eq!(
            clock.start_of(date("2026-10-10")),
            utc("2026-10-09T22:00:00Z")
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
    async fn an_unpaired_link_lives_in_utc_from_four() {
        let (link, _dir) = crate::testing::offline("terra").await;
        assert_eq!(link.time_zone(), Tz::UTC);
        assert_eq!(Clock::of(&link).await, Clock::default());
    }
}
