use super::{fork::Capture, variables::Value, Shell};
use crate::{
    parser::{Expander, Select},
    sys::{self, env as sys_env, variables as self_sys},
    types,
};
use std::{env, io::Read, iter::FromIterator, process};

impl<'a, 'b> Expander for Shell<'b> {
    /// Uses a subshell to expand a given command.
    fn command(&self, command: &str) -> Option<types::Str> {
        let output = match self
            .fork(Capture::StdoutThenIgnoreStderr, move |shell| shell.on_command(command))
        {
            Ok(result) => {
                let mut string = String::with_capacity(1024);
                match result.stdout.unwrap().read_to_string(&mut string) {
                    Ok(_) => Some(string),
                    Err(why) => {
                        eprintln!("ion: error reading stdout of child: {}", why);
                        None
                    }
                }
            }
            Err(why) => {
                eprintln!("ion: fork error: {}", why);
                None
            }
        };

        // Ensure that the parent retains ownership of the terminal before exiting.
        let _ = sys::tcsetpgrp(sys::STDIN_FILENO, process::id());
        output.map(Into::into)
    }

    /// Expand a string variable given if its quoted / unquoted
    fn string(&self, name: &str) -> Option<types::Str> {
        if name == "?" {
            Some(types::Str::from(self.previous_status.to_string()))
        } else {
            self.variables().get_str(name)
        }
    }

    /// Expand an array variable with some selection
    fn array(&self, name: &str, selection: &Select) -> Option<types::Args> {
        match self.variables.get_ref(name) {
            Some(Value::Array(array)) => match selection {
                Select::All => {
                    Some(types::Args::from_iter(array.iter().map(|x| format!("{}", x).into())))
                }
                Select::Index(ref id) => id
                    .resolve(array.len())
                    .and_then(|n| array.get(n))
                    .map(|x| args![types::Str::from(format!("{}", x))]),
                Select::Range(ref range) => {
                    range.bounds(array.len()).and_then(|(start, length)| {
                        if array.len() > start {
                            Some(
                                array
                                    .iter()
                                    .skip(start)
                                    .take(length)
                                    .map(|var| format!("{}", var).into())
                                    .collect(),
                            )
                        } else {
                            None
                        }
                    })
                }
                _ => None,
            },
            Some(Value::HashMap(hmap)) => match selection {
                Select::All => {
                    let mut array = types::Args::new();
                    for (key, value) in hmap.iter() {
                        array.push(key.clone());
                        let f = format!("{}", value);
                        match *value {
                            Value::Str(_) => array.push(f.into()),
                            Value::Array(_) | Value::HashMap(_) | Value::BTreeMap(_) => {
                                for split in f.split_whitespace() {
                                    array.push(split.into());
                                }
                            }
                            _ => (),
                        }
                    }
                    Some(array)
                }
                Select::Key(key) => {
                    Some(args![format!("{}", hmap.get(&*key).unwrap_or(&Value::Str("".into())))])
                }
                Select::Index(index) => {
                    use crate::ranges::Index;
                    Some(args![format!(
                        "{}",
                        hmap.get(&types::Str::from(
                            match index {
                                Index::Forward(n) => *n as isize,
                                Index::Backward(n) => -((*n + 1) as isize),
                            }
                            .to_string()
                        ))
                        .unwrap_or(&Value::Str("".into()))
                    )])
                }
                _ => None,
            },
            Some(Value::BTreeMap(bmap)) => match selection {
                Select::All => {
                    let mut array = types::Args::new();
                    for (key, value) in bmap.iter() {
                        array.push(key.clone());
                        let f = format!("{}", value);
                        match *value {
                            Value::Str(_) => array.push(f.into()),
                            Value::Array(_) | Value::HashMap(_) | Value::BTreeMap(_) => {
                                for split in f.split_whitespace() {
                                    array.push(split.into());
                                }
                            }
                            _ => (),
                        }
                    }
                    Some(array)
                }
                Select::Key(key) => {
                    Some(args![format!("{}", bmap.get(&*key).unwrap_or(&Value::Str("".into())))])
                }
                Select::Index(index) => {
                    use crate::ranges::Index;
                    Some(args![format!(
                        "{}",
                        bmap.get(&types::Str::from(
                            match index {
                                Index::Forward(n) => *n as isize,
                                Index::Backward(n) => -((*n + 1) as isize),
                            }
                            .to_string()
                        ))
                        .unwrap_or(&Value::Str("".into()))
                    )])
                }
                _ => None,
            },
            _ => None,
        }
    }

    fn map_keys(&self, name: &str, sel: &Select) -> Option<types::Args> {
        match self.variables.get_ref(name) {
            Some(&Value::HashMap(ref map)) => {
                Self::select(map.keys().map(|x| format!("{}", x).into()), sel, map.len())
            }
            Some(&Value::BTreeMap(ref map)) => {
                Self::select(map.keys().map(|x| format!("{}", x).into()), sel, map.len())
            }
            _ => None,
        }
    }

    fn map_values(&self, name: &str, sel: &Select) -> Option<types::Args> {
        match self.variables.get_ref(name) {
            Some(&Value::HashMap(ref map)) => {
                Self::select(map.values().map(|x| format!("{}", x).into()), sel, map.len())
            }
            Some(&Value::BTreeMap(ref map)) => {
                Self::select(map.values().map(|x| format!("{}", x).into()), sel, map.len())
            }
            _ => None,
        }
    }

    fn tilde(&self, input: &str) -> Option<String> {
        // Only if the first character is a tilde character will we perform expansions
        if !input.starts_with('~') {
            return None;
        }

        let separator = input[1..].find(|c| c == '/' || c == '$');
        let (tilde_prefix, rest) = input[1..].split_at(separator.unwrap_or(input.len() - 1));

        match tilde_prefix {
            "" => sys_env::home_dir().map(|home| home.to_string_lossy().to_string() + rest),
            "+" => Some(env::var("PWD").unwrap_or_else(|_| "?".to_string()) + rest),
            "-" => self.variables.get_str("OLDPWD").map(|oldpwd| oldpwd.to_string() + rest),
            _ => {
                let (neg, tilde_num) = if tilde_prefix.starts_with('+') {
                    (false, &tilde_prefix[1..])
                } else if tilde_prefix.starts_with('-') {
                    (true, &tilde_prefix[1..])
                } else {
                    (false, tilde_prefix)
                };

                match tilde_num.parse() {
                    Ok(num) => if neg {
                        self.directory_stack.dir_from_top(num)
                    } else {
                        self.directory_stack.dir_from_bottom(num)
                    }
                    .map(|path| path.to_str().unwrap().to_string()),
                    Err(_) => self_sys::get_user_home(tilde_prefix).map(|home| home + rest),
                }
            }
        }
    }
}