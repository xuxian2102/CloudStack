//! 磁盘文本文件和内存编辑 buffer 之间的无损格式合同：换行符风格和末尾是否
//! 有换行。跟 UI 框架、`PostDocument`、Git 都无关——纯字节 ↔ 字符串转换。
//!
//! 合同：
//! ```text
//! 磁盘 "hello\r\nworld\r\n"
//!   → decode_text →
//! 内存 "hello\nworld\n"（内部永远只用 LF），format.line_ending = CrLf，
//! format.has_final_newline = true（读取时观察到的事实）
//!   → encode_text(line_ending) →
//! 磁盘 "hello\r\nworld\r\n"（原样恢复）
//! ```
//!
//! `TextFileFormat::has_final_newline` **只是 `decode_text` 读盘时记录的元数据**
//! （供诊断、格式变化检测等场景使用），`encode_text` 不会拿它去强制加/去
//! `text` 末尾的换行——末尾有没有换行本来就已经完整地体现在 `text` 自身
//! 的内容里（`decode_text` 从不改动它），而且末尾换行在源码编辑器里是可
//! 编辑正文的一部分，不是保存层可以替用户决定的格式属性：用户在 EOF 按
//! Backspace 删除末尾换行，或者按 Enter 加一个，`encode_text` 都必须原样
//! 保留 `text` 已经反映出的结果，不能拿旧的 `has_final_newline` 覆盖回去。
//! 同理，`text` 末尾多个连续换行（额外空行）也必须原样保留，不能因为
//! `has_final_newline = true` 就被规范化成只剩一个。
//!
//! 换行风格不一致（`LineEnding::Mixed`，包括单独出现、不成对的 `\r`）的文件
//! 仍然可以正常打开——buffer 内一律归一化成 LF，不维护逐行的原始换行映射；
//! 保存时不尝试恢复原来的混合模式，统一规范化为 LF。第一版故意选择"归一化
//! 到 LF"而不是"归一化到项目里出现次数最多的那种"，避免把配置系统拖进这
//! 一步——deterministic 的结果更容易验证，以后如果需要"多数优先"可以在这
//! 个合同之上单独加一层。

use crate::AppError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineEnding {
    Lf,
    CrLf,
    Mixed,
}

/// 读盘时观察到的原始格式。`has_final_newline` 只是记录事实，不是保存时
/// 的强制策略——`encode_text` 不接收它，也不会用它覆盖 `text` 当前的
/// 末尾状态，见模块文档。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextFileFormat {
    pub line_ending: LineEnding,
    pub has_final_newline: bool,
}

/// `text` 内部统一使用 `\n`，不管磁盘上原来是什么换行风格。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedText {
    pub text: String,
    pub format: TextFileFormat,
}

/// 把磁盘字节解码成统一 LF 的字符串 + 检测到的原始格式。字节必须是合法
/// UTF-8——CloudStack 的文章和草稿本来就只支持 UTF-8 文本，非法编码直接
/// 报错，不做有损猜测式解码。
pub fn decode_text(bytes: &[u8]) -> Result<DecodedText, AppError> {
    let raw = std::str::from_utf8(bytes)
        .map_err(|error| AppError::Io(format!("文本不是合法的 UTF-8：{error}")))?;

    let has_final_newline = raw.ends_with('\n') || raw.ends_with('\r');

    let mut has_crlf = false;
    let mut has_lone_lf = false;
    let mut has_lone_cr = false;
    let raw_bytes = raw.as_bytes();
    let mut i = 0;
    while i < raw_bytes.len() {
        match raw_bytes[i] {
            b'\r' if raw_bytes.get(i + 1) == Some(&b'\n') => {
                has_crlf = true;
                i += 2;
                continue;
            }
            b'\r' => has_lone_cr = true,
            b'\n' => has_lone_lf = true,
            _ => {}
        }
        i += 1;
    }

    // 单独出现、不成对的 `\r`（老式 Mac 换行）永远算作 Mixed——它既不是
    // 纯 LF 也不是纯 CRLF；两种成对/不成对的换行同时出现同样是 Mixed。
    // 什么换行都没有（单行文件、空文件）时没有证据可判断，默认 Lf。
    let line_ending = if has_lone_cr || (has_crlf && has_lone_lf) {
        LineEnding::Mixed
    } else if has_crlf {
        LineEnding::CrLf
    } else {
        LineEnding::Lf
    };

    let text = raw.replace("\r\n", "\n").replace('\r', "\n");

    Ok(DecodedText {
        text,
        format: TextFileFormat {
            line_ending,
            has_final_newline,
        },
    })
}

/// 把内存里的字符串按 `line_ending` 编码回磁盘字节。只做 EOL 风格转换：
/// 末尾有没有换行、有几个连续空行，完全由 `text` 自身的内容决定——不接收
/// `TextFileFormat`，因为 `has_final_newline` 是读盘时的元数据，不是保存
/// 指令，见模块文档。`text` 不要求调用方保证已经是纯 LF——防御性地先归
/// 一化一遍，任何残留的 `\r\n`/`\r` 都会被当成 LF 处理。
pub fn encode_text(text: &str, line_ending: LineEnding) -> Result<Vec<u8>, AppError> {
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    let encoded = match line_ending {
        LineEnding::CrLf => normalized.replace('\n', "\r\n"),
        LineEnding::Lf | LineEnding::Mixed => normalized,
    };
    Ok(encoded.into_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crlf_with_final_newline_round_trips_losslessly() {
        let disk = b"hello\r\nworld\r\n";
        let decoded = decode_text(disk).unwrap();

        assert_eq!(decoded.text, "hello\nworld\n");
        assert_eq!(decoded.format.line_ending, LineEnding::CrLf);
        assert!(decoded.format.has_final_newline);

        let encoded = encode_text(&decoded.text, decoded.format.line_ending).unwrap();
        assert_eq!(encoded, disk);
    }

    #[test]
    fn lf_without_final_newline_round_trips_losslessly() {
        let disk = b"hello\nworld";
        let decoded = decode_text(disk).unwrap();

        assert_eq!(decoded.text, "hello\nworld");
        assert_eq!(decoded.format.line_ending, LineEnding::Lf);
        assert!(!decoded.format.has_final_newline);

        let encoded = encode_text(&decoded.text, decoded.format.line_ending).unwrap();
        assert_eq!(encoded, disk);
    }

    #[test]
    fn empty_file_round_trips_as_lf_without_final_newline() {
        let decoded = decode_text(b"").unwrap();
        assert_eq!(decoded.text, "");
        assert_eq!(decoded.format.line_ending, LineEnding::Lf);
        assert!(!decoded.format.has_final_newline);

        let encoded = encode_text(&decoded.text, decoded.format.line_ending).unwrap();
        assert_eq!(encoded, b"");
    }

    #[test]
    fn single_line_without_any_newline_defaults_to_lf() {
        let decoded = decode_text(b"no newline here").unwrap();
        assert_eq!(decoded.format.line_ending, LineEnding::Lf);
        assert!(!decoded.format.has_final_newline);
    }

    #[test]
    fn mixed_line_endings_are_detected_and_normalized_to_lf_in_memory() {
        let decoded = decode_text(b"hello\r\nworld\nagain\r\n").unwrap();
        assert_eq!(decoded.text, "hello\nworld\nagain\n");
        assert_eq!(decoded.format.line_ending, LineEnding::Mixed);
        assert!(decoded.format.has_final_newline);
    }

    #[test]
    fn bare_cr_without_a_following_lf_counts_as_mixed() {
        // 老式 Mac 换行：单独的 \r，不跟 \n 配对。
        let decoded = decode_text(b"hello\rworld\r").unwrap();
        assert_eq!(decoded.text, "hello\nworld\n");
        assert_eq!(decoded.format.line_ending, LineEnding::Mixed);
        assert!(decoded.format.has_final_newline);
    }

    #[test]
    fn mixed_line_endings_are_normalized_to_lf_on_encode_not_restored() {
        let decoded = decode_text(b"hello\r\nworld\nagain").unwrap();
        assert_eq!(decoded.format.line_ending, LineEnding::Mixed);

        // 保存时不尝试恢复原始的混合换行模式，统一规范化为 LF。
        let encoded = encode_text(&decoded.text, decoded.format.line_ending).unwrap();
        assert_eq!(encoded, b"hello\nworld\nagain");
    }

    #[test]
    fn decode_rejects_invalid_utf8() {
        assert!(decode_text(&[0xff, 0xfe, 0xfd]).is_err());
    }

    #[test]
    fn encode_is_defensive_against_non_normalized_input() {
        // 即使调用方传进来的 text 不是纯 LF（比如上层忘了先归一化），
        // encode 也不应该产出错误编码的换行。
        let encoded = encode_text("hello\r\nworld\n", LineEnding::Lf).unwrap();
        assert_eq!(encoded, b"hello\nworld\n");
    }

    #[test]
    fn encode_preserves_absent_final_newline_from_text() {
        assert_eq!(encode_text("hello", LineEnding::CrLf).unwrap(), b"hello");
    }

    #[test]
    fn encode_preserves_multiple_trailing_newlines_from_text() {
        assert_eq!(
            encode_text("hello\n\n", LineEnding::CrLf).unwrap(),
            b"hello\r\n\r\n"
        );
    }

    #[test]
    fn encode_does_not_reintroduce_a_final_newline_the_user_deleted() {
        let decoded = decode_text(b"hello\n").unwrap();
        assert!(decoded.format.has_final_newline);

        // 用户在编辑器里用 Backspace 删掉了末尾换行；has_final_newline 仍然
        // 是读盘时的旧事实，encode 不能拿它把删掉的换行加回来。
        let edited = decoded.text.trim_end_matches('\n').to_owned();
        let encoded = encode_text(&edited, decoded.format.line_ending).unwrap();
        assert_eq!(encoded, b"hello");
    }

    #[test]
    fn encode_does_not_strip_a_final_newline_the_user_added() {
        let decoded = decode_text(b"hello").unwrap();
        assert!(!decoded.format.has_final_newline);

        // 用户在 EOF 按了 Enter；has_final_newline 仍然是读盘时"没有"的旧
        // 事实，encode 不能拿它把用户刚输入的换行删掉。
        let edited = format!("{}\n", decoded.text);
        let encoded = encode_text(&edited, decoded.format.line_ending).unwrap();
        assert_eq!(encoded, b"hello\n");
    }
}
