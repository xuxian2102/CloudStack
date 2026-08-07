//! Frontmatter 编辑器控件用到的值规则：标签输入怎么解析、日历日期是否真实
//! 存在、一个月有多少天。不依赖任何 UI 框架——GTK 控件的可选日期范围
//! （比如"只能选 2000 年到今天"）不属于这里，那是编辑器控件自己的展示策略。

use std::collections::HashSet;

use chrono::NaiveDate;

pub fn parse_tags_input(text: &str) -> Vec<String> {
    let mut seen = HashSet::new();

    text.split(',')
        .map(str::trim)
        .filter(|tag| !tag.is_empty())
        .filter(|tag| seen.insert((*tag).to_owned()))
        .map(str::to_owned)
        .collect()
}

pub fn parse_calendar_date(value: &str) -> Option<(i32, u32, u32)> {
    let mut parts = value.split('-');

    let year = parts.next()?.parse().ok()?;
    let month = parts.next()?.parse().ok()?;
    let day = parts.next()?.parse().ok()?;

    if parts.next().is_some() {
        return None;
    }

    NaiveDate::from_ymd_opt(year, month, day)?;

    Some((year, month, day))
}

pub fn days_in_month(year: i32, month: u32) -> Option<u32> {
    let first = NaiveDate::from_ymd_opt(year, month, 1)?;

    let (next_year, next_month) = if month == 12 {
        (year.checked_add(1)?, 1)
    } else {
        (year, month + 1)
    };

    let next = NaiveDate::from_ymd_opt(next_year, next_month, 1)?;
    u32::try_from((next - first).num_days()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tag_input_is_trimmed_deduplicated_and_empty_values_removed() {
        assert_eq!(
            parse_tags_input(" rust, GTK, rust, ,中文 "),
            ["rust", "GTK", "中文"]
        );
    }

    #[test]
    fn calendar_date_rejects_invalid_dates() {
        assert_eq!(parse_calendar_date("2024-02-29"), Some((2024, 2, 29)));
        assert_eq!(parse_calendar_date("2025-02-29"), None);
        assert_eq!(parse_calendar_date("2026-11-31"), None);
        assert_eq!(parse_calendar_date("2026-00-01"), None);
        assert_eq!(parse_calendar_date("2026-13-01"), None);
    }

    #[test]
    fn month_lengths_follow_the_gregorian_calendar() {
        assert_eq!(days_in_month(2000, 2), Some(29));
        assert_eq!(days_in_month(1900, 2), Some(28));
        assert_eq!(days_in_month(2026, 4), Some(30));
        assert_eq!(days_in_month(2026, 12), Some(31));
        assert_eq!(days_in_month(2026, 0), None);
        assert_eq!(days_in_month(2026, 13), None);
    }
}
