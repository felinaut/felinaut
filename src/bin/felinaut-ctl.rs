//! Command-line client for the agent control server of Felinaut (`src/agent.rs`).

use std::{
    env, fs,
    io::{BufRead, BufReader, Write},
    os::unix::net::UnixStream,
    path::PathBuf,
    process::exit,
};

use serde_json::{Value, json};

const USAGE: &str = "\
Usage: felinaut-ctl <command>      (start Felinaut with FELINAUT_AGENT=1)

  tabs                    List the tabs
  tab <index>             Select a tab
  navigate <url|search>   Load a page in the active tab
  snapshot                List the visible elements that accept input, with references
  click <@ref>            Click an element
  type <@ref> <text>      Replace the value of a field
  press <key>             Press a key, for example Enter, Tab, Escape, ArrowDown, a
  eval <expression>       Evaluate one JavaScript expression in the page. For statements,
                          use a function: (() => { …; return x })()
  screenshot [path]       Save a PNG of the page
  live                    List the live folders, their sections and items
  live add <integration>  Add a live folder (github, asana)
  live refresh|remove <index>
                          Refresh or remove a live folder
  live open|close <folder> <section> <item>
                          Show a live folder item in its tab, or close it until it is updated
  downloads               List the downloads: name, state, bytes and path
  downloads show          Open or close the downloads list
  downloads cancel <id>   Stop a running download

  --native                With click, type or press: send a real mouse or keyboard event.
                          Use it when a site ignores the default synthetic events.";

fn command(args: &[String]) -> Option<Value> {
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    Some(match args[..] {
        ["tabs"] | ["snapshot"] => json!({"cmd": args[0]}),
        ["tab", index] => json!({"cmd": "tab", "index": index.parse::<u64>().ok()?}),
        ["live"] => json!({"cmd": "live"}),
        ["live", "add", key] => json!({"cmd": "live", "add": key}),
        ["live", action @ ("refresh" | "remove"), index] => {
            json!({"cmd": "live", action: index.parse::<u64>().ok()?})
        }
        ["live", action @ ("open" | "close"), folder, section, item] => json!({
            "cmd": "live",
            "open": [folder.parse::<u64>().ok()?, section.parse::<u64>().ok()?, item.parse::<u64>().ok()?],
            "close": action == "close",
        }),
        ["downloads"] => json!({"cmd": "downloads"}),
        ["downloads", "show"] => json!({"cmd": "downloads", "show": true}),
        ["downloads", "cancel", id] => {
            json!({"cmd": "downloads", "cancel": id.parse::<u64>().ok()?})
        }
        ["navigate", url] => json!({"cmd": "navigate", "url": url}),
        ["click", reference] => json!({"cmd": "click", "ref": reference}),
        ["type", reference, text] => json!({"cmd": "type", "ref": reference, "text": text}),
        ["press", key] => json!({"cmd": "press", "key": key}),
        ["eval", code] => json!({"cmd": "eval", "code": code}),
        // The server writes the file, thus the path must be absolute.
        ["screenshot"] => json!({
            "cmd": "screenshot",
            "path": env::temp_dir().join("felinaut-screenshot.png"),
        }),
        ["screenshot", path] => json!({
            "cmd": "screenshot",
            "path": std::path::absolute(path).ok()?,
        }),
        _ => return None,
    })
}

fn send(mut command: Value) -> Result<Value, String> {
    // The same directory as `agent::dir`.
    let dir = PathBuf::from(env::var_os("HOME").ok_or("No home directory")?)
        .join("Library/Application Support/Felifox");
    let offline = |_| "Felinaut does not listen. Start it with FELINAUT_AGENT=1.".to_string();
    command["token"] = fs::read_to_string(dir.join("agent.token"))
        .map_err(offline)?
        .into();
    let mut stream = UnixStream::connect(dir.join("agent.sock")).map_err(offline)?;
    let error = |error: std::io::Error| error.to_string();
    stream
        .write_all(format!("{command}\n").as_bytes())
        .map_err(error)?;
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line).map_err(error)?;
    let answer: Value = serde_json::from_str(&line).map_err(|_| "Felinaut gave no answer")?;
    if answer["ok"] == true {
        Ok(answer["value"].clone())
    } else {
        Err(answer["error"].as_str().unwrap_or("Unknown error").into())
    }
}

/// One line for each element: `@e3 textbox "Search" = "cats"`.
fn print_snapshot(page: &Value) {
    println!(
        "{} | {}",
        page["url"].as_str().unwrap_or_default(),
        page["title"].as_str().unwrap_or_default()
    );
    for element in page["elements"].as_array().into_iter().flatten() {
        let mut line = format!(
            "{} {} {}",
            element["ref"].as_str().unwrap_or_default(),
            element["role"].as_str().unwrap_or_default(),
            element["name"]
        );
        if !element["value"].is_null() {
            line += &format!(" = {}", element["value"]);
        }
        println!("{line}");
    }
}

fn main() {
    let mut args: Vec<String> = env::args().skip(1).collect();
    let native = args.iter().any(|arg| arg == "--native");
    args.retain(|arg| arg != "--native");
    let Some(mut command) = command(&args) else {
        eprintln!("{USAGE}");
        exit(2);
    };
    let name = command["cmd"].clone();
    command["native"] = native.into();
    match send(command) {
        Ok(value) if name == "snapshot" => print_snapshot(&value),
        Ok(Value::Null) => {}
        Ok(Value::String(text)) => println!("{text}"),
        Ok(value) => println!("{value:#}"),
        Err(error) => {
            eprintln!("felinaut-ctl: {error}");
            exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Option<Value> {
        command(&args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>())
    }

    #[test]
    fn renamed_client_preserves_navigation_and_live_item_commands() {
        assert_eq!(
            parse(&["navigate", "felinaut://newtab"]),
            Some(json!({"cmd": "navigate", "url": "felinaut://newtab"}))
        );
        assert_eq!(
            parse(&["live", "open", "0", "1", "2"]),
            Some(json!({"cmd": "live", "open": [0, 1, 2], "close": false}))
        );
        assert_eq!(
            parse(&["live", "close", "0", "1", "2"]),
            Some(json!({"cmd": "live", "open": [0, 1, 2], "close": true}))
        );
        assert_eq!(parse(&["downloads"]), Some(json!({"cmd": "downloads"})));
        assert_eq!(
            parse(&["downloads", "show"]),
            Some(json!({"cmd": "downloads", "show": true}))
        );
        assert_eq!(
            parse(&["downloads", "cancel", "3"]),
            Some(json!({"cmd": "downloads", "cancel": 3}))
        );
        for args in [
            vec![],
            vec!["unknown"],
            vec!["tab", "-1"],
            vec!["live", "open", "0", "x", "2"],
            vec!["downloads", "cancel"],
            vec!["downloads", "cancel", "all"],
            vec!["downloads", "open", "1"],
        ] {
            assert_eq!(parse(&args), None);
        }
    }

    #[test]
    fn default_screenshot_uses_the_new_name_without_changing_custom_paths() {
        let default = parse(&["screenshot"]).unwrap();
        assert_eq!(
            default["path"],
            env::temp_dir()
                .join("felinaut-screenshot.png")
                .to_string_lossy()
                .as_ref()
        );
        let custom = parse(&["screenshot", "/tmp/custom.png"]).unwrap();
        assert_eq!(custom["path"], "/tmp/custom.png");
    }
}
