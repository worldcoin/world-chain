use testcontainers::core::logs::LogFrame;
use tracing::{Level, event};

macro_rules! emit_at_level {
    ($target:literal, $level:expr, $process:expr, $line:expr) => {
        match $level {
            Level::ERROR => event!(target: $target, Level::ERROR, process = %$process, "{}", $line),
            Level::WARN => event!(target: $target, Level::WARN, process = %$process, "{}", $line),
            Level::INFO => event!(target: $target, Level::INFO, process = %$process, "{}", $line),
            Level::DEBUG => event!(target: $target, Level::DEBUG, process = %$process, "{}", $line),
            Level::TRACE => event!(target: $target, Level::TRACE, process = %$process, "{}", $line),
        }
    };
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum ProcessLogTarget {
    L1DevChain,
    WorldChainEl,
    OpDeployer,
    OpNode,
    OpConductor,
    OpBatcher,
    OpProposer,
    OpChallenger,
    OpStackService,
    Prometheus,
    Grafana,
}

pub(crate) fn container_log_consumer(
    id: impl Into<String>,
    target: ProcessLogTarget,
) -> impl Fn(&LogFrame) + Send + Sync + 'static {
    let id = id.into();
    move |frame| {
        let bytes = match frame {
            LogFrame::StdOut(bytes) | LogFrame::StdErr(bytes) => bytes,
        };
        let text = String::from_utf8_lossy(bytes);
        for line in text.lines().filter(|line| !line.trim().is_empty()) {
            emit_process_log(target, &id, line);
        }
    }
}

pub(crate) fn emit_process_log(target: ProcessLogTarget, process: &str, line: &str) {
    let line = strip_log_timestamp(line);
    let level = normalize_process_level(target, line, parse_log_level(line));
    let line = strip_log_level_fields(line);
    match target {
        ProcessLogTarget::L1DevChain => {
            emit_at_level!("l1_dev_chain", level, process, line.as_str())
        }
        ProcessLogTarget::WorldChainEl => {
            emit_at_level!("world_chain_el", level, process, line.as_str())
        }
        ProcessLogTarget::OpDeployer => {
            emit_at_level!("op_deployer", level, process, line.as_str())
        }
        ProcessLogTarget::OpNode => emit_at_level!("op_node", level, process, line.as_str()),
        ProcessLogTarget::OpConductor => {
            emit_at_level!("op_conductor", level, process, line.as_str())
        }
        ProcessLogTarget::OpBatcher => emit_at_level!("op_batcher", level, process, line.as_str()),
        ProcessLogTarget::OpProposer => {
            emit_at_level!("op_proposer", level, process, line.as_str())
        }
        ProcessLogTarget::OpChallenger => {
            emit_at_level!("op_challenger", level, process, line.as_str())
        }
        ProcessLogTarget::OpStackService => {
            emit_at_level!("op_stack_service", level, process, line.as_str())
        }
        ProcessLogTarget::Prometheus => emit_at_level!("prometheus", level, process, line.as_str()),
        ProcessLogTarget::Grafana => emit_at_level!("grafana", level, process, line.as_str()),
    }
}

fn normalize_process_level(target: ProcessLogTarget, line: &str, level: Level) -> Level {
    if matches!(target, ProcessLogTarget::OpNode)
        && matches!(level, Level::ERROR)
        && line.contains("Sequencer encountered reset signal, aborting work")
        && line.contains("cannot continue derivation until Engine has been reset")
    {
        return Level::WARN;
    }

    level
}

pub(crate) fn strip_log_timestamp(line: &str) -> &str {
    line.strip_prefix("ts=")
        .or_else(|| line.strip_prefix("t="))
        .and_then(|rest| rest.split_once(' ').map(|(_, line)| line))
        .unwrap_or(line)
}

fn parse_log_level(line: &str) -> Level {
    logfmt_field_ranges(line)
        .find_map(|range| {
            let field = &line[range];
            field
                .strip_prefix("level=")
                .or_else(|| field.strip_prefix("lvl="))
        })
        .map(|value| value.trim_matches(['"', '\'']).to_ascii_lowercase())
        .and_then(|value| match value.as_str() {
            "error" | "eror" | "crit" | "critical" => Some(Level::ERROR),
            "warn" | "warning" => Some(Level::WARN),
            "info" => Some(Level::INFO),
            "debug" | "dbug" => Some(Level::DEBUG),
            "trace" | "trce" => Some(Level::TRACE),
            _ => None,
        })
        .unwrap_or(Level::INFO)
}

fn strip_log_level_fields(line: &str) -> String {
    logfmt_field_ranges(line)
        .filter_map(|range| {
            let field = &line[range.clone()];
            (!field.starts_with("level=") && !field.starts_with("lvl=")).then_some(field)
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn logfmt_field_ranges(line: &str) -> impl Iterator<Item = std::ops::Range<usize>> + '_ {
    let mut ranges = Vec::new();
    let mut start = None;
    let mut in_quotes = false;
    let mut escaped = false;

    for (index, ch) in line.char_indices() {
        if start.is_none() {
            if ch.is_whitespace() {
                continue;
            }
            start = Some(index);
        }

        if ch == '"' && !escaped {
            in_quotes = !in_quotes;
        }

        if ch.is_whitespace()
            && !in_quotes
            && let Some(start) = start.take()
        {
            ranges.push(start..index);
        }

        escaped = ch == '\\' && !escaped;
        if ch != '\\' {
            escaped = false;
        }
    }

    if let Some(start) = start {
        ranges.push(start..line.len());
    }

    ranges.into_iter()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_level_and_lvl_fields() {
        assert_eq!(parse_log_level("level=info msg=ready"), Level::INFO);
        assert_eq!(parse_log_level("lvl=error msg=failed"), Level::ERROR);
        assert_eq!(parse_log_level("lvl=warn msg=slow"), Level::WARN);
        assert_eq!(
            parse_log_level(r#"msg="contains lvl=error text" lvl=warn"#),
            Level::WARN
        );
    }

    #[test]
    fn strips_common_log_timestamps() {
        assert_eq!(
            strip_log_timestamp("ts=2026-05-21T00:00:00Z level=info msg=ready"),
            "level=info msg=ready"
        );
        assert_eq!(
            strip_log_timestamp("t=2026-05-21T00:00:00Z lvl=info msg=ready"),
            "lvl=info msg=ready"
        );
    }

    #[test]
    fn strips_log_level_fields_after_level_detection() {
        assert_eq!(
            strip_log_level_fields(
                r#"lvl=warn msg="Using dev L1 genesis without any customization""#
            ),
            r#"msg="Using dev L1 genesis without any customization""#
        );
        assert_eq!(
            strip_log_level_fields(r#"level=info target=engine message="ready""#),
            r#"target=engine message="ready""#
        );
        assert_eq!(
            strip_log_level_fields(r#"msg="contains lvl=warn text" lvl=warn"#),
            r#"msg="contains lvl=warn text""#
        );
    }

    #[test]
    fn demotes_expected_op_node_startup_reset() {
        assert_eq!(
            normalize_process_level(
                ProcessLogTarget::OpNode,
                r#"lvl=error msg="Sequencer encountered reset signal, aborting work" err="reset: cannot continue derivation until Engine has been reset""#,
                Level::ERROR,
            ),
            Level::WARN
        );
        assert_eq!(
            normalize_process_level(
                ProcessLogTarget::OpBatcher,
                r#"lvl=error msg="Sequencer encountered reset signal, aborting work" err="reset: cannot continue derivation until Engine has been reset""#,
                Level::ERROR,
            ),
            Level::ERROR
        );
    }
}
