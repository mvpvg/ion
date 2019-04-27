pub mod functions;
pub mod source;
pub mod variables;

mod command_info;
mod exec;
mod exists;
mod is;
mod job_control;
mod man_pages;
mod set;
mod status;

use ion_builtins::{calc, conditionals, echo, random, test};

use self::{
    command_info::*,
    echo::echo,
    exec::exec,
    exists::exists,
    functions::fn_,
    is::is,
    man_pages::*,
    source::source,
    status::status,
    test::test,
    variables::{alias, drop_alias, drop_array, drop_variable},
};

use std::{
    error::Error,
    io::{self, Write},
    os::unix::io::RawFd,
};

use crate::{
    shell::{
        self,
        fork_function::fork_function,
        job_control::{JobControl, ProcessState},
        status::*,
        Shell, ShellHistory,
    },
    sys, types,
};
use small;

const HELP_DESC: &str = "Display helpful information about a given command or list commands if \
                         none specified\n    help <command>";

const SOURCE_DESC: &str = "Evaluate the file following the command or re-initialize the init file";

const DISOWN_DESC: &str =
    "Disowning a process removes that process from the shell's background process table.";

/// The type for builtin functions. Builtins have direct access to the shell
pub type BuiltinFunction = fn(&[small::String], &mut Shell) -> i32;

macro_rules! map {
    ($($name:expr => $func:ident: $help:expr),+) => {{
        BuiltinMap {
            name: &[$($name),+],
            help: &[$($help),+],
            functions: &[$($func),+],
        }
    }
}}

/// If you are implementing a builtin add it to the table below, create a well named manpage in
/// man_pages and check for help flags by adding to the start of your builtin the following
/// if check_help(args, MAN_BUILTIN_NAME) {
///     return SUCCESS
/// }

/// Builtins are in A-Z order.
pub const BUILTINS: &BuiltinMap = &map!(
    "alias" => builtin_alias : "View, set or unset aliases",
    "bg" => builtin_bg : "Resumes a stopped background process",
    "bool" => builtin_bool : "If the value is '1' or 'true', return 0 exit status",
    "calc" => builtin_calc : "Calculate a mathematical expression",
    "cd" => builtin_cd : "Change the current directory\n    cd <path>",
    "contains" => contains : "Evaluates if the supplied argument contains a given string",
    "dirs" => builtin_dirs : "Display the current directory stack",
    "disown" => builtin_disown : DISOWN_DESC,
    "drop" => builtin_drop : "Delete a variable",
    "echo" => builtin_echo : "Display a line of text",
    "ends-with" => ends_with : "Evaluates if the supplied argument ends with a given string",
    "eq" => builtin_eq : "Simple alternative to == and !=",
    "eval" => builtin_eval : "Evaluates the evaluated expression",
    "exec" => builtin_exec : "Replace the shell with the given command.",
    "exists" => builtin_exists : "Performs tests on files and text",
    "exit" => builtin_exit : "Exits the current session",
    "false" => builtin_false : "Do nothing, unsuccessfully",
    "fg" => builtin_fg : "Resumes and sets a background process as the active process",
    "fn" => builtin_fn : "Print list of functions",
    "help" => builtin_help : HELP_DESC,
    "history" => builtin_history : "Display a log of all commands previously executed",
    "is" => builtin_is : "Simple alternative to == and !=",
    "isatty" => builtin_isatty : "Returns 0 exit status if the supplied FD is a tty",
    "jobs" => builtin_jobs : "Displays all jobs that are attached to the background",
    "matches" => builtin_matches : "Checks if a string matches a given regex",
    "popd" => builtin_popd : "Pop a directory from the stack",
    "pushd" => builtin_pushd : "Push a directory to the stack",
    "random" => builtin_random : "Outputs a random u64",
    "read" => builtin_read : "Read some variables\n    read <variable>",
    "set" => builtin_set : "Set or unset values of shell options and positional parameters.",
    "source" => builtin_source : SOURCE_DESC,
    "starts-with" => starts_with : "Evaluates if the supplied argument starts with a given string",
    "status" => builtin_status : "Evaluates the current runtime status",
    "suspend" => builtin_suspend : "Suspends the shell with a SIGTSTOP signal",
    "test" => builtin_test : "Performs tests on files and text",
    "true" => builtin_true : "Do nothing, successfully",
    "type" => builtin_type : "indicates how a command would be interpreted",
    "unalias" => builtin_unalias : "Delete an alias",
    "wait" => builtin_wait : "Waits until all running background processes have completed",
    "which" => builtin_which : "Shows the full path of commands"
);

pub struct BuiltinMap {
    pub(crate) name:      &'static [&'static str],
    pub(crate) help:      &'static [&'static str],
    pub(crate) functions: &'static [BuiltinFunction],
}

impl BuiltinMap {
    pub fn contains_key(&self, func: &str) -> bool { self.name.binary_search(&func).is_ok() }

    pub fn keys(&self) -> &'static [&'static str] { self.name }

    pub fn get_help<'a>(&self, func: &'a str) -> Option<&'static str> {
        self.name.binary_search(&func).ok().map(|pos| unsafe { *self.help.get_unchecked(pos) })
    }

    pub fn get<'a>(&self, func: &'a str) -> Option<BuiltinFunction> {
        self.name.binary_search(&func).ok().map(|pos| unsafe { *self.functions.get_unchecked(pos) })
    }
}

fn starts_with(args: &[small::String], _: &mut Shell) -> i32 { conditionals::starts_with(args) }
fn ends_with(args: &[small::String], _: &mut Shell) -> i32 { conditionals::ends_with(args) }
fn contains(args: &[small::String], _: &mut Shell) -> i32 { conditionals::contains(args) }

// Definitions of simple builtins go here
fn builtin_status(args: &[small::String], shell: &mut Shell) -> i32 {
    match status(args, shell) {
        Ok(()) => SUCCESS,
        Err(why) => {
            let stderr = io::stderr();
            let mut stderr = stderr.lock();
            let _ = stderr.write_all(why.as_bytes());
            FAILURE
        }
    }
}

pub fn builtin_cd(args: &[small::String], shell: &mut Shell) -> i32 {
    if check_help(args, MAN_CD) {
        return SUCCESS;
    }

    match shell.directory_stack.cd(args, &shell.variables) {
        Ok(()) => {
            fork_function(shell, "CD_CHANGE", &["ion"]);
            SUCCESS
        }
        Err(why) => {
            eprintln!("{}", why);
            FAILURE
        }
    }
}

fn builtin_bool(args: &[small::String], shell: &mut Shell) -> i32 {
    if args.len() != 2 {
        let stderr = io::stderr();
        let mut stderr = stderr.lock();
        let _ = stderr.write_all(b"bool requires one argument\n");
        return FAILURE;
    }

    let opt = shell.variables.get::<types::Str>(&args[1][1..]);
    let sh_var: &str = match opt.as_ref() {
        Some(s) => s,
        None => "",
    };

    match sh_var {
        "1" => (),
        "true" => (),
        _ => match &*args[1] {
            "1" => (),
            "true" => (),
            "--help" => println!("{}", MAN_BOOL),
            "-h" => println!("{}", MAN_BOOL),
            _ => return FAILURE,
        },
    }
    SUCCESS
}

fn builtin_is(args: &[small::String], shell: &mut Shell) -> i32 {
    if check_help(args, MAN_IS) {
        return SUCCESS;
    }

    match is(args, shell) {
        Ok(()) => SUCCESS,
        Err(why) => {
            let stderr = io::stderr();
            let mut stderr = stderr.lock();
            let _ = stderr.write_all(why.as_bytes());
            FAILURE
        }
    }
}

fn builtin_dirs(args: &[small::String], shell: &mut Shell) -> i32 {
    if check_help(args, MAN_DIRS) {
        return SUCCESS;
    }

    shell.directory_stack.dirs(args.iter())
}

fn builtin_pushd(args: &[small::String], shell: &mut Shell) -> i32 {
    if check_help(args, MAN_PUSHD) {
        return SUCCESS;
    }
    match shell.directory_stack.pushd(args, &mut shell.variables) {
        Ok(()) => SUCCESS,
        Err(why) => {
            let stderr = io::stderr();
            let mut stderr = stderr.lock();
            let _ = stderr.write_all(why.as_bytes());
            FAILURE
        }
    }
}

fn builtin_popd(args: &[small::String], shell: &mut Shell) -> i32 {
    if check_help(args, MAN_POPD) {
        return SUCCESS;
    }
    match shell.directory_stack.popd(args) {
        Ok(()) => SUCCESS,
        Err(why) => {
            let stderr = io::stderr();
            let mut stderr = stderr.lock();
            let _ = stderr.write_all(why.as_bytes());
            FAILURE
        }
    }
}

fn builtin_alias(args: &[small::String], shell: &mut Shell) -> i32 {
    let args_str = args[1..].join(" ");
    alias(&mut shell.variables, &args_str)
}

fn builtin_unalias(args: &[small::String], shell: &mut Shell) -> i32 {
    drop_alias(&mut shell.variables, args)
}

// TODO There is a man page for fn however the -h and --help flags are not
// checked for.
fn builtin_fn(_: &[small::String], shell: &mut Shell) -> i32 { fn_(&mut shell.variables) }

fn builtin_read(args: &[small::String], shell: &mut Shell) -> i32 {
    if check_help(args, MAN_READ) {
        return SUCCESS;
    }
    shell.variables.read(args)
}

fn builtin_drop(args: &[small::String], shell: &mut Shell) -> i32 {
    if check_help(args, MAN_DROP) {
        return SUCCESS;
    }
    if args.len() >= 2 && args[1] == "-a" {
        drop_array(&mut shell.variables, args)
    } else {
        drop_variable(&mut shell.variables, args)
    }
}

fn builtin_set(args: &[small::String], shell: &mut Shell) -> i32 {
    if check_help(args, MAN_SET) {
        return SUCCESS;
    }
    set::set(args, shell)
}

fn builtin_eq(args: &[small::String], shell: &mut Shell) -> i32 {
    if check_help(args, MAN_EQ) {
        return SUCCESS;
    }

    match is(args, shell) {
        Ok(()) => SUCCESS,
        Err(why) => {
            let stderr = io::stderr();
            let mut stderr = stderr.lock();
            let _ = stderr.write_all(why.as_bytes());
            FAILURE
        }
    }
}

fn builtin_eval(args: &[small::String], shell: &mut Shell) -> i32 {
    if check_help(args, MAN_EVAL) {
        SUCCESS
    } else {
        shell.execute_command(args[1..].join(" ").as_bytes()).unwrap_or_else(|_| {
            eprintln!("ion: supplied eval expression was not terminated");
            FAILURE
        })
    }
}

fn builtin_history(args: &[small::String], shell: &mut Shell) -> i32 {
    if check_help(args, MAN_HISTORY) {
        return SUCCESS;
    }
    shell.print_history(args)
}

fn builtin_source(args: &[small::String], shell: &mut Shell) -> i32 {
    if check_help(args, MAN_SOURCE) {
        return SUCCESS;
    }
    match source(shell, args) {
        Ok(()) => SUCCESS,
        Err(why) => {
            let stderr = io::stderr();
            let mut stderr = stderr.lock();
            let _ = stderr.write_all(why.as_bytes());
            FAILURE
        }
    }
}

fn builtin_echo(args: &[small::String], _: &mut Shell) -> i32 {
    if check_help(args, MAN_ECHO) {
        return SUCCESS;
    }
    match echo(args) {
        Ok(()) => SUCCESS,
        Err(why) => {
            let stderr = io::stderr();
            let mut stderr = stderr.lock();
            let _ = stderr.write_all(why.description().as_bytes());
            FAILURE
        }
    }
}

fn builtin_test(args: &[small::String], _: &mut Shell) -> i32 {
    // Do not use `check_help` for the `test` builtin. The
    // `test` builtin contains a "-h" option.
    match test(args) {
        Ok(true) => SUCCESS,
        Ok(false) => FAILURE,
        Err(why) => {
            eprintln!("{}", why);
            FAILURE
        }
    }
}

// TODO create manpage.
fn builtin_calc(args: &[small::String], _: &mut Shell) -> i32 {
    match calc::calc(&args[1..]) {
        Ok(()) => SUCCESS,
        Err(why) => {
            eprintln!("{}", why);
            FAILURE
        }
    }
}

fn builtin_random(args: &[small::String], _: &mut Shell) -> i32 {
    if check_help(args, MAN_RANDOM) {
        return SUCCESS;
    }
    match random::random(&args[1..]) {
        Ok(()) => SUCCESS,
        Err(why) => {
            eprintln!("{}", why);
            FAILURE
        }
    }
}

fn builtin_true(args: &[small::String], _: &mut Shell) -> i32 {
    check_help(args, MAN_TRUE);
    SUCCESS
}

fn builtin_false(args: &[small::String], _: &mut Shell) -> i32 {
    if check_help(args, MAN_FALSE) {
        return SUCCESS;
    }
    FAILURE
}

// TODO create a manpage
fn builtin_wait(_: &[small::String], shell: &mut Shell) -> i32 {
    shell.wait_for_background();
    SUCCESS
}

fn builtin_jobs(args: &[small::String], shell: &mut Shell) -> i32 {
    check_help(args, MAN_JOBS);
    job_control::jobs(shell);
    SUCCESS
}

fn builtin_bg(args: &[small::String], shell: &mut Shell) -> i32 {
    if check_help(args, MAN_BG) {
        return SUCCESS;
    }
    job_control::bg(shell, &args[1..])
}

fn builtin_fg(args: &[small::String], shell: &mut Shell) -> i32 {
    if check_help(args, MAN_FG) {
        return SUCCESS;
    }
    job_control::fg(shell, &args[1..])
}

fn builtin_suspend(args: &[small::String], _: &mut Shell) -> i32 {
    if check_help(args, MAN_SUSPEND) {
        return SUCCESS;
    }
    shell::signals::suspend(0);
    SUCCESS
}

fn builtin_disown(args: &[small::String], shell: &mut Shell) -> i32 {
    for arg in args {
        if *arg == "--help" {
            println!("{}", MAN_DISOWN);
            return SUCCESS;
        }
    }
    match job_control::disown(shell, &args[1..]) {
        Ok(()) => SUCCESS,
        Err(err) => {
            eprintln!("ion: disown: {}", err);
            FAILURE
        }
    }
}

fn builtin_help(args: &[small::String], shell: &mut Shell) -> i32 {
    let builtins = shell.builtins;
    let stdout = io::stdout();
    let mut stdout = stdout.lock();
    if let Some(command) = args.get(1) {
        if let Some(help) = builtins.get_help(command) {
            let _ = stdout.write_all(help.as_bytes());
            let _ = stdout.write_all(b"\n");
        } else {
            let _ = stdout.write_all(b"Command helper not found [run 'help']...");
            let _ = stdout.write_all(b"\n");
        }
    } else {
        let commands = builtins.keys();

        let mut buffer: Vec<u8> = Vec::new();
        for command in commands {
            let _ = writeln!(buffer, "{}", command);
        }
        let _ = stdout.write_all(&buffer);
    }
    SUCCESS
}

fn builtin_exit(args: &[small::String], shell: &mut Shell) -> i32 {
    if check_help(args, MAN_EXIT) {
        return SUCCESS;
    }
    // Kill all active background tasks before exiting the shell.
    for process in shell.background.lock().unwrap().iter() {
        if process.state != ProcessState::Empty {
            let _ = sys::kill(process.pid, sys::SIGTERM);
        }
    }
    let previous_status = shell.previous_status;
    shell.exit(args.get(1).and_then(|status| status.parse::<i32>().ok()).unwrap_or(previous_status))
}

fn builtin_exec(args: &[small::String], shell: &mut Shell) -> i32 {
    match exec(shell, &args[1..]) {
        // Shouldn't ever hit this case.
        Ok(()) => SUCCESS,
        Err(err) => {
            let stderr = io::stderr();
            let mut stderr = stderr.lock();
            let _ = writeln!(stderr, "ion: exec: {}", err);
            FAILURE
        }
    }
}

use regex::Regex;
fn builtin_matches(args: &[small::String], _: &mut Shell) -> i32 {
    if check_help(args, MAN_MATCHES) {
        return SUCCESS;
    }
    if args[1..].len() != 2 {
        let stderr = io::stderr();
        let mut stderr = stderr.lock();
        let _ = stderr.write_all(b"match takes two arguments\n");
        return BAD_ARG;
    }
    let input = &args[1];
    let re = match Regex::new(&args[2]) {
        Ok(r) => r,
        Err(e) => {
            let stderr = io::stderr();
            let mut stderr = stderr.lock();
            let _ = stderr
                .write_all(format!("couldn't compile input regex {}: {}\n", args[2], e).as_bytes());
            return FAILURE;
        }
    };

    if re.is_match(input) {
        SUCCESS
    } else {
        FAILURE
    }
}

fn builtin_exists(args: &[small::String], shell: &mut Shell) -> i32 {
    if check_help(args, MAN_EXISTS) {
        return SUCCESS;
    }
    match exists(args, shell) {
        Ok(true) => SUCCESS,
        Ok(false) => FAILURE,
        Err(why) => {
            eprintln!("{}", why);
            FAILURE
        }
    }
}

fn builtin_which(args: &[small::String], shell: &mut Shell) -> i32 {
    match which(args, shell) {
        Ok(result) => result,
        Err(()) => FAILURE,
    }
}

fn builtin_type(args: &[small::String], shell: &mut Shell) -> i32 {
    match find_type(args, shell) {
        Ok(result) => result,
        Err(()) => FAILURE,
    }
}

fn builtin_isatty(args: &[small::String], _: &mut Shell) -> i32 {
    if check_help(args, MAN_ISATTY) {
        return SUCCESS;
    }

    if args.len() > 1 {
        // sys::isatty expects a usize if compiled for redox but otherwise a i32.
        match args[1].parse::<RawFd>() {
            Ok(r) => {
                if sys::isatty(r) {
                    return SUCCESS;
                }
            }
            Err(_) => eprintln!("ion: isatty given bad number"),
        }
    } else {
        return SUCCESS;
    }

    FAILURE
}
