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
    let level = parse_log_level(line);
    match target {
        ProcessLogTarget::L1DevChain => emit_at_level!("l1_dev_chain", level, process, line),
        ProcessLogTarget::WorldChainEl => emit_at_level!("world_chain_el", level, process, line),
        ProcessLogTarget::OpDeployer => emit_at_level!("op_deployer", level, process, line),
        ProcessLogTarget::OpNode => emit_at_level!("op_node", level, process, line),
        ProcessLogTarget::OpConductor => emit_at_level!("op_conductor", level, process, line),
        ProcessLogTarget::OpBatcher => emit_at_level!("op_batcher", level, process, line),
        ProcessLogTarget::OpProposer => emit_at_level!("op_proposer", level, process, line),
        ProcessLogTarget::OpChallenger => emit_at_level!("op_challenger", level, process, line),
        ProcessLogTarget::OpStackService => {
            emit_at_level!("op_stack_service", level, process, line)
        }
        ProcessLogTarget::Prometheus => emit_at_level!("prometheus", level, process, line),
        ProcessLogTarget::Grafana => emit_at_level!("grafana", level, process, line),
    }
}

pub(crate) fn strip_log_timestamp(line: &str) -> &str {
    line.strip_prefix("ts=")
        .or_else(|| line.strip_prefix("t="))
        .and_then(|rest| rest.split_once(' ').map(|(_, line)| line))
        .unwrap_or(line)
}

fn parse_log_level(line: &str) -> Level {
    line.split_whitespace()
        .find_map(|field| {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_level_and_lvl_fields() {
        assert_eq!(parse_log_level("level=info msg=ready"), Level::INFO);
        assert_eq!(parse_log_level("lvl=error msg=failed"), Level::ERROR);
        assert_eq!(parse_log_level("lvl=warn msg=slow"), Level::WARN);
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
}
