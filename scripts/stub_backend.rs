// Stand-in for aria2c and yt-dlp in black-box tests. It records its argument
// vector (and the first bytes of any argument naming an existing file) as one
// JSON line in $STUB_LOG, then exits with $STUB_EXIT (default 0).
use std::{env, fs, io::Read, io::Write};

fn quote(text: &str) -> String {
    let mut out = String::from("\"");
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    let listed: Vec<String> = args.iter().map(|a| quote(a)).collect();
    let mut heads = Vec::new();
    for arg in &args {
        if let Ok(mut file) = fs::File::open(arg) {
            let mut head = [0u8; 16];
            let count = file.read(&mut head).unwrap_or(0);
            heads.push(format!(
                "{}:{}",
                quote(arg),
                quote(&String::from_utf8_lossy(&head[..count]))
            ));
        }
    }
    if let Ok(path) = env::var("STUB_LOG") {
        let mut log = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .expect("open STUB_LOG");
        writeln!(
            log,
            "{{\"program\":{},\"args\":[{}],\"files\":{{{}}}}}",
            quote(&env::args().next().unwrap_or_default()),
            listed.join(","),
            heads.join(",")
        )
        .unwrap();
    }
    println!("stub backend ran");
    std::process::exit(
        env::var("STUB_EXIT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0),
    );
}
