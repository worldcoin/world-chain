use std::path::Path;

pub fn from_out_dir(out_dir: &Path) -> Result<&str, &'static str> {
    if out_dir.file_name().is_none_or(|name| name != "out") {
        return Err("OUT_DIR did not end in Cargo's out directory");
    }

    out_dir
        .ancestors()
        .find(|path| path.file_name().is_some_and(|name| name == "build"))
        .and_then(Path::parent)
        .and_then(Path::file_name)
        .and_then(|profile| profile.to_str())
        .ok_or("OUT_DIR did not include a Cargo profile before build")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supports_both_cargo_package_layouts() {
        for profile in ["debug", "release", "maxperf"] {
            for package in ["world-chain-node-a1b2", "world-chain-node/a1b2"] {
                let out_dir = format!("/target/{profile}/build/{package}/out");
                assert_eq!(from_out_dir(Path::new(&out_dir)), Ok(profile));
            }
        }
    }

    #[test]
    fn supports_custom_target_directories_and_target_triples() {
        let out_dir = Path::new(
            "/custom/build/target/aarch64-unknown-linux-gnu/maxperf/build/world-chain-node/a1b2/out",
        );
        assert_eq!(from_out_dir(out_dir), Ok("maxperf"));
    }

    #[test]
    fn rejects_unrecognized_layouts() {
        for out_dir in [
            "out",
            "/out",
            "/build/world-chain-node/a1b2/out",
            "/target/maxperf/world-chain-node/a1b2/out",
            "/target/maxperf/build/world-chain-node/a1b2/not-out",
        ] {
            assert!(from_out_dir(Path::new(out_dir)).is_err(), "{out_dir}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn rejects_non_utf8_profiles() {
        use std::{ffi::OsString, os::unix::ffi::OsStringExt};

        let out_dir = Path::new("/target")
            .join(OsString::from_vec(vec![0xff]))
            .join("build/world-chain-node/a1b2/out");
        assert!(from_out_dir(&out_dir).is_err());
    }
}
