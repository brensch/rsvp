//! Formal-stationery wording: "Saturday, the twelfth of June",
//! "two thousand and twenty-seven", "half past four in the afternoon".

use time::{Date, Month, PrimitiveDateTime, Weekday};

const ONES: [&str; 20] = [
    "zero", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten", "eleven", "twelve",
    "thirteen", "fourteen", "fifteen", "sixteen", "seventeen", "eighteen", "nineteen",
];
const TENS: [&str; 10] = ["", "", "twenty", "thirty", "forty", "fifty", "sixty", "seventy", "eighty", "ninety"];

pub fn number(n: u32) -> String {
    match n {
        0..=19 => ONES[n as usize].into(),
        20..=99 => {
            let t = TENS[(n / 10) as usize];
            if n % 10 == 0 { t.into() } else { format!("{t}-{}", ONES[(n % 10) as usize]) }
        }
        100..=999 => {
            let h = format!("{} hundred", ONES[(n / 100) as usize]);
            if n % 100 == 0 { h } else { format!("{h} and {}", number(n % 100)) }
        }
        _ => {
            let th = format!("{} thousand", number(n / 1000));
            match n % 1000 {
                0 => th,
                r if r < 100 => format!("{th} and {}", number(r)),
                r => format!("{th} {}", number(r)),
            }
        }
    }
}

pub fn ordinal(n: u32) -> String {
    let irregular = |w: &str| -> Option<&'static str> {
        Some(match w {
            "one" => "first",
            "two" => "second",
            "three" => "third",
            "five" => "fifth",
            "eight" => "eighth",
            "nine" => "ninth",
            "twelve" => "twelfth",
            _ => return None,
        })
    };
    let w = number(n);
    let (head, last) = match w.rfind('-') {
        Some(i) => (&w[..=i], &w[i + 1..]),
        None => ("", w.as_str()),
    };
    let last = if let Some(irr) = irregular(last) {
        irr.to_string()
    } else if let Some(stem) = last.strip_suffix('y') {
        format!("{stem}ieth")
    } else {
        format!("{last}th")
    };
    format!("{head}{last}")
}

pub fn month_name(m: Month) -> &'static str {
    match m {
        Month::January => "January",
        Month::February => "February",
        Month::March => "March",
        Month::April => "April",
        Month::May => "May",
        Month::June => "June",
        Month::July => "July",
        Month::August => "August",
        Month::September => "September",
        Month::October => "October",
        Month::November => "November",
        Month::December => "December",
    }
}

pub fn weekday_name(w: Weekday) -> &'static str {
    match w {
        Weekday::Monday => "Monday",
        Weekday::Tuesday => "Tuesday",
        Weekday::Wednesday => "Wednesday",
        Weekday::Thursday => "Thursday",
        Weekday::Friday => "Friday",
        Weekday::Saturday => "Saturday",
        Weekday::Sunday => "Sunday",
    }
}

/// "Saturday, the twelfth of June"
pub fn date_words(d: Date) -> String {
    format!("{}, the {} of {}", weekday_name(d.weekday()), ordinal(d.day() as u32), month_name(d.month()))
}

/// "two thousand and twenty-seven"
pub fn year_words(d: Date) -> String {
    number(d.year() as u32)
}

/// "Saturday 12 June 2027"
pub fn date_long(d: Date) -> String {
    format!("{} {} {} {}", weekday_name(d.weekday()), d.day(), month_name(d.month()), d.year())
}

/// "12 June 2027"
pub fn date_medium(d: Date) -> String {
    format!("{} {} {}", d.day(), month_name(d.month()), d.year())
}

/// "12 · 06 · 27"
pub fn date_dots(d: Date) -> String {
    format!("{:02} · {:02} · {:02}", d.day(), d.month() as u8, d.year() % 100)
}

/// "3:00 pm" / "3 pm"
pub fn time_short(t: PrimitiveDateTime) -> String {
    let (h, m) = (t.hour(), t.minute());
    let h12 = match h % 12 {
        0 => 12,
        x => x,
    };
    let ap = if h < 12 { "am" } else { "pm" };
    if m == 0 { format!("{h12} {ap}") } else { format!("{h12}:{m:02} {ap}") }
}

/// "three o'clock in the afternoon", "half past four in the afternoon"
pub fn time_words(t: PrimitiveDateTime) -> String {
    let (h, m) = (t.hour() as u32, t.minute() as u32);
    let h12 = match h % 12 {
        0 => 12,
        x => x,
    };
    let part = match h {
        0..=11 => "in the morning",
        12..=16 => "in the afternoon",
        _ => "in the evening",
    };
    if h == 12 && m == 0 {
        return "twelve o'clock noon".into();
    }
    let core = match m {
        0 => format!("{} o'clock", number(h12)),
        15 => format!("quarter past {}", number(h12)),
        30 => format!("half past {}", number(h12)),
        _ => format!("{} {}", number(h12), if m < 10 { format!("oh {}", number(m)) } else { number(m) }),
    };
    format!("{core} {part}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::{date, datetime};

    #[test]
    fn wording() {
        assert_eq!(number(2027), "two thousand and twenty-seven");
        assert_eq!(number(2030), "two thousand and thirty");
        assert_eq!(ordinal(12), "twelfth");
        assert_eq!(ordinal(21), "twenty-first");
        assert_eq!(ordinal(30), "thirtieth");
        assert_eq!(ordinal(23), "twenty-third");
        assert_eq!(date_words(date!(2027 - 06 - 12)), "Saturday, the twelfth of June");
        assert_eq!(time_words(datetime!(2027-06-12 15:00)), "three o'clock in the afternoon");
        assert_eq!(time_words(datetime!(2027-06-12 16:30)), "half past four in the afternoon");
        assert_eq!(time_short(datetime!(2027-06-12 15:30)), "3:30 pm");
    }
}
