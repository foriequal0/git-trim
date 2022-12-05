use git_trim::args::Args;

use clap::{Command, CommandFactory};
use man::prelude::*;

fn main() {
    let command: Command = <Args as CommandFactory>::command();

    let mut page = Manual::new(command.get_name()).flag(
        Flag::new()
            .short("-h")
            .long("--help")
            .help("Prints help information"),
    );

    if let Some(about) = command.get_about() {
        page = page.about(about.to_string());
    }

    for arg in command.get_arguments() {
        let hidden = arg.is_hide_set();
        if hidden {
            continue;
        }

        let name = arg.get_id().as_str();
        let short_help = arg.get_help();
        let long_help = arg.get_long_help();
        let help = match (short_help, long_help) {
            (None, None) => None,
            (Some(help), None) | (None, Some(help)) => Some(help),
            (Some(_), Some(long_help)) => Some(long_help),
        };
        let short = arg.get_short();
        let long = arg.get_long();
        let flag = !arg.get_action().takes_values();
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
                    flag = flag.help(&help.to_string());
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
                    opt = opt.help(&help.to_string());
                }
                opt
            });
        }
    }

    println!("{}", page.render());
}
