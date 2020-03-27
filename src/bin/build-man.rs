use git_trim::args::Args;

use clap::{App, ArgSettings, IntoApp};
use man::prelude::*;
use regex::Regex;
use rson_rs::value::Value;

fn main() {
    let app: App = <Args as IntoApp>::into_app();

    let mut page = Manual::new(&app.name).flag(
        Flag::new()
            .short("-h")
            .long("--help")
            .help("Prints help information"),
    );

    if let Some(about) = app.about {
        page = page.about(about);
    }

    let malformed_values = Regex::new("(id|settings|validator): .+?, ").unwrap();
    for arg in app.args.args {
        let printed = malformed_values
            .replace_all(&format!("{:?}", arg), "")
            .replace("\\'", "'")
            .to_string();
        let parsed = match Value::from_str(&printed) {
            Ok(value) => value,
            Err(err) => panic!("{:?}", err),
        };

        let hidden = arg.is_set(ArgSettings::Hidden);
        if hidden {
            continue;
        }

        let name = arg.name;
        let short_help = arg.help;
        let long_help = get_string(&parsed, "long_help");
        let help = match (short_help, long_help) {
            (None, None) => None,
            (Some(help), None) | (None, Some(help)) => Some(help),
            (Some(_), Some(long_help)) => Some(long_help),
        };
        let short = arg.short;
        let long = arg.long;
        let flag = !arg.is_set(ArgSettings::TakesValue);
        if flag {
            page = page.flag({
                let mut flag = Flag::new();
                if let Some(short) = short {
                    flag = flag.short(&format!("-{}", short))
                }
                if let Some(long) = long {
                    flag = flag.long(&format!("--{}", long));
                }
                if let Some(help) = help {
                    flag = flag.help(&help);
                }
                flag
            });
        } else {
            page = page.option({
                let mut opt = Opt::new(name);
                if let Some(short) = short {
                    opt = opt.short(&format!("-{}", short))
                }
                if let Some(long) = long {
                    opt = opt.long(&format!("--{}", long));
                }
                if let Some(help) = help {
                    opt = opt.help(&help);
                }
                opt
            });
        }
    }

    println!("{}", page.render());
}

fn get_string<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    let map = match value {
        Value::Map(map) => map,
        _ => panic!("top level should be a map"),
    };
    match &map[&Value::String(key.to_string())] {
        Value::String(string) => return Some(string.as_str()),
        Value::Option(None) => return None,
        Value::Option(Some(value)) => {
            if let Value::String(string) = value.as_ref() {
                return Some(string.as_str());
            }
        }
        _ => {}
    }
    panic!("key not exist: {}", key)
}
