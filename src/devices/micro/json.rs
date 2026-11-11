use std::fmt::Write as _;

use serde_json::Value;
use unicode_general_category::{GeneralCategory, get_general_category};

pub(crate) fn python_whitespace(c: char) -> bool {
    c.is_whitespace() || matches!(c, '\u{1c}'..='\u{1f}')
}

pub(crate) fn numeric(value: &Value) -> Option<f64> {
    let Value::Number(number) = value else {
        return None;
    };
    let number = number.to_string().parse::<f64>().ok()?;
    number.is_finite().then_some(number)
}

pub(crate) fn python_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(v) => *v,
        Value::Number(v) => v.to_string().parse::<f64>().is_ok_and(|n| n != 0.0),
        Value::String(v) => !v.is_empty(),
        Value::Array(v) => !v.is_empty(),
        Value::Object(v) => !v.is_empty(),
    }
}

pub(crate) fn numeric_equals_u64(value: &Value, expected: u64) -> bool {
    match value {
        Value::Bool(v) => u64::from(*v) == expected,
        Value::Number(number) => {
            let raw = number.to_string();
            if raw.contains(['.', 'e', 'E']) {
                raw.parse::<f64>().is_ok_and(|number| {
                    number.is_finite()
                        && (0.0..18_446_744_073_709_551_616.0).contains(&number)
                        && number as u64 == expected
                        && number.fract() == 0.0
                })
            } else if raw == "-0" {
                expected == 0
            } else {
                raw.parse::<u64>() == Ok(expected)
            }
        }
        _ => false,
    }
}

pub(crate) fn category_c(c: char) -> bool {
    matches!(
        get_general_category(c),
        GeneralCategory::Control
            | GeneralCategory::Format
            | GeneralCategory::Surrogate
            | GeneralCategory::PrivateUse
            | GeneralCategory::Unassigned
    )
}

fn python_printable(c: char) -> bool {
    c == ' '
        || (!category_c(c)
            && !matches!(
                get_general_category(c),
                GeneralCategory::SpaceSeparator
                    | GeneralCategory::LineSeparator
                    | GeneralCategory::ParagraphSeparator
            ))
}

pub(crate) fn repr_string(value: &str) -> String {
    let quote = if value.contains('\'') && !value.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut result = String::new();
    result.push(quote);
    for c in value.chars() {
        match c {
            '\\' => result.push_str("\\\\"),
            '\t' => result.push_str("\\t"),
            '\n' => result.push_str("\\n"),
            '\r' => result.push_str("\\r"),
            c if c == quote => {
                result.push('\\');
                result.push(c);
            }
            c if python_printable(c) => result.push(c),
            c if (c as u32) <= 0xff => {
                write!(result, "\\x{:02x}", c as u32).expect("writing to a string cannot fail");
            }
            c if (c as u32) <= 0xffff => {
                write!(result, "\\u{:04x}", c as u32).expect("writing to a string cannot fail");
            }
            c => {
                write!(result, "\\U{:08x}", c as u32).expect("writing to a string cannot fail");
            }
        }
    }
    result.push(quote);
    result
}

pub(crate) fn repr_float(value: f64) -> String {
    if value.is_nan() {
        return "nan".to_owned();
    }
    if value.is_infinite() {
        return if value.is_sign_negative() {
            "-inf"
        } else {
            "inf"
        }
        .to_owned();
    }
    if value == 0.0 {
        return if value.is_sign_negative() {
            "-0.0"
        } else {
            "0.0"
        }
        .to_owned();
    }

    let mut buffer = ryu::Buffer::new();
    let raw = buffer.format_finite(value.abs());
    let (mantissa, exponent) = raw
        .split_once('e')
        .map_or((raw, 0), |(m, e)| (m, e.parse::<i32>().unwrap_or(0)));
    let dot = mantissa.find('.').unwrap_or(mantissa.len());
    let mut digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let leading = digits.bytes().take_while(|c| *c == b'0').count();
    let exponent = exponent + dot as i32 - 1 - leading as i32;
    digits.drain(..leading);
    while digits.len() > 1 && digits.ends_with('0') {
        digits.pop();
    }

    let mut result = if value.is_sign_negative() {
        String::from("-")
    } else {
        String::new()
    };
    if (-4..=15).contains(&exponent) {
        if exponent < 0 {
            result.push_str("0.");
            result.extend(std::iter::repeat_n('0', (-exponent - 1) as usize));
            result.push_str(&digits);
        } else {
            let whole = exponent as usize + 1;
            if digits.len() <= whole {
                result.push_str(&digits);
                result.extend(std::iter::repeat_n('0', whole - digits.len()));
                result.push_str(".0");
            } else {
                result.push_str(&digits[..whole]);
                result.push('.');
                result.push_str(&digits[whole..]);
            }
        }
    } else {
        result.push(digits.as_bytes()[0] as char);
        if digits.len() > 1 {
            result.push('.');
            result.push_str(&digits[1..]);
        }
        write!(
            result,
            "e{}{:02}",
            if exponent < 0 { '-' } else { '+' },
            exponent.unsigned_abs()
        )
        .expect("writing to a string cannot fail");
    }
    result
}

fn render_number(number: &serde_json::Number) -> String {
    let raw = number.to_string();
    if raw.contains(['.', 'e', 'E']) {
        raw.parse::<f64>().map_or(raw, repr_float)
    } else if raw == "-0" {
        "0".to_owned()
    } else {
        raw
    }
}

pub(crate) fn append_json_string(output: &mut String, text: &str) -> Result<(), ()> {
    output.push_str(&serde_json::to_string(text).map_err(|_| ())?);
    Ok(())
}

enum RenderItem<'a> {
    Value(&'a Value),
    Text(&'static str),
    String(&'a str),
}

fn render(output: &mut String, value: &Value, python: bool) -> Result<(), ()> {
    let mut pending = vec![RenderItem::Value(value)];
    while let Some(item) = pending.pop() {
        match item {
            RenderItem::Text(text) => output.push_str(text),
            RenderItem::String(text) => {
                if python {
                    output.push_str(&repr_string(text));
                } else {
                    append_json_string(output, text)?;
                }
            }
            RenderItem::Value(Value::Null) => {
                output.push_str(if python { "None" } else { "null" });
            }
            RenderItem::Value(Value::Bool(value)) => {
                output.push_str(match (*value, python) {
                    (true, true) => "True",
                    (false, true) => "False",
                    (true, false) => "true",
                    (false, false) => "false",
                });
            }
            RenderItem::Value(Value::Number(number)) => {
                if python {
                    output.push_str(&render_number(number));
                } else {
                    let raw = number.to_string();
                    if raw.contains(['.', 'e', 'E']) {
                        let number = raw.parse::<f64>().map_err(|_| ())?;
                        if !number.is_finite() {
                            return Err(());
                        }
                        output.push_str(&repr_float(number));
                    } else if raw == "-0" {
                        output.push('0');
                    } else {
                        output.push_str(&raw);
                    }
                }
            }
            RenderItem::Value(Value::String(text)) => pending.push(RenderItem::String(text)),
            RenderItem::Value(Value::Array(values)) => {
                output.push('[');
                pending.push(RenderItem::Text("]"));
                for (index, value) in values.iter().enumerate().rev() {
                    pending.push(RenderItem::Value(value));
                    if index != 0 {
                        pending.push(RenderItem::Text(if python { ", " } else { "," }));
                    }
                }
            }
            RenderItem::Value(Value::Object(values)) => {
                output.push('{');
                pending.push(RenderItem::Text("}"));
                let mut entries: Vec<_> = values.iter().collect();
                entries.sort_unstable_by_key(|(key, _)| *key);
                for (index, (key, value)) in entries.into_iter().enumerate().rev() {
                    pending.push(RenderItem::Value(value));
                    pending.push(RenderItem::Text(if python { ": " } else { ":" }));
                    pending.push(RenderItem::String(key));
                    if index != 0 {
                        pending.push(RenderItem::Text(if python { ", " } else { "," }));
                    }
                }
            }
        }
    }
    Ok(())
}

pub(crate) fn repr_value(value: &Value) -> String {
    let mut result = String::new();
    render(&mut result, value, true).expect("Python rendering cannot fail");
    result
}

pub(crate) fn str_value(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        _ => repr_value(value),
    }
}

pub(crate) fn append_json(output: &mut String, value: &Value) -> Result<(), ()> {
    render(output, value, false)
}
