//! The command table: every built-in the runtime registers with the script engine.
//!
//! `runtime.s2l` holds the commands (`Abs`, `View`, `Mid$`, `FileOpen`, …) as objects,
//! each with an initialiser that hands the engine its name, how many arguments it
//! takes, and a descriptor of the argument lists it accepts. The vendor's compiler
//! looks a call up in this table, case-insensitively, and checks the call against it.
//! `tools/command_table.py` reads the registrations out of the file and writes
//! `commands_table.rs`; `docs/script.md` says how.
//!
//! The table is in registration order, and that order is part of the interface: the
//! engine numbers a command 0x7a plus its place in it, and the first word a script's own
//! symbols can use follows the last command.
//!
//! What is here is the vendor's own description of the interface, not an
//! implementation. Nothing in it runs a command: that is the interpreter's job, and
//! each command's behaviour still has to be read from `runtime.s2l`.

mod table {
    include!("commands_table.rs");
}

/// What a command returns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ret {
    Void,
    Int,
    Real,
    Str,
}

/// One argument type word as the vendor stores it. The low seven bits are the type
/// (at most 0x13), bits 8 to 12 the number of array dimensions, and 0x80 a flag whose
/// meaning is not recovered. `0xe` and `0xf` open and close a group of arguments that
/// are written back; the group counts as one argument.
pub type ArgWord = u16;

pub const GROUP_OPEN: ArgWord = 0xe;
pub const GROUP_CLOSE: ArgWord = 0xf;

#[derive(Clone, Copy, Debug)]
pub struct Command {
    pub name: &'static str,
    pub min_args: u8,
    pub max_args: u8,
    pub ret: Ret,
    /// The argument lists the command accepts, one per variant.
    pub variants: &'static [&'static [ArgWord]],
}

impl Command {
    /// The type of an argument word, without the dimension bits or the flag.
    pub fn arg_type(word: ArgWord) -> u8 {
        (word & 0x7f) as u8
    }
    /// How many array dimensions the word asks for.
    pub fn arg_dims(word: ArgWord) -> u8 {
        ((word >> 8) & 0x1f) as u8
    }
}

/// All commands, in the order the engine registers them.
pub fn all() -> impl Iterator<Item = Command> {
    table::TABLE.iter().map(|&(name, min_args, max_args, ret, variants)| Command {
        name,
        min_args,
        max_args,
        ret: match ret {
            1 => Ret::Int,
            2 => Ret::Real,
            3 => Ret::Str,
            _ => Ret::Void,
        },
        variants,
    })
}

/// The word the engine gives the command at `index` in [`all`].
pub fn word(index: usize) -> u16 {
    0x7a + index as u16
}

/// How many commands there are.
pub fn count() -> usize {
    table::TABLE.len()
}

/// The command a script name refers to, and its place in [`all`]. Names compare without
/// regard to case, as they do in the vendor's compiler, which lower-cases a name before
/// it looks it up.
pub fn find(name: &str) -> Option<(usize, Command)> {
    all().enumerate().find(|(_, c)| c.name.eq_ignore_ascii_case(name))
}

pub fn lookup(name: &str) -> Option<Command> {
    find(name).map(|(_, c)| c)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_table_holds_what_the_runtime_registers() {
        assert_eq!(all().count(), 553);
        let mut names: Vec<_> = all().map(|c| c.name.to_ascii_lowercase()).collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), 553, "names are unique without regard to case");
    }

    #[test]
    fn a_command_is_numbered_from_0x7a_in_registration_order() {
        // Checked against compiler.dll with the table registered in this order: Eval is
        // the first command, Sound the second, Abs the 430th.
        assert_eq!(find("Eval").map(|(i, _)| word(i)), Some(0x7a));
        assert_eq!(find("Sound").map(|(i, _)| word(i)), Some(0x7b));
        assert_eq!(find("Abs").map(|(i, _)| word(i)), Some(551));
        assert_eq!(find("Mid$").map(|(i, _)| word(i)), Some(516));
        assert_eq!(find("Len").map(|(i, _)| word(i)), Some(447));
        assert_eq!(find("XHigh").map(|(i, _)| word(i)), Some(164));
    }

    #[test]
    fn lookup_ignores_case() {
        let abs = lookup("aBs").unwrap();
        assert_eq!((abs.min_args, abs.max_args, abs.ret), (1, 1, Ret::Real));
        assert_eq!(abs.variants, &[&[17u16][..]]);
        assert_eq!(lookup("Mid$").unwrap().ret, Ret::Str);
        assert!(lookup("NoSuchCommand").is_none());
    }

    /// The vendor accepts a registration only if its argument counts agree with the
    /// descriptor (compiler.dll 0x180005c80). Hold the table to the same rule.
    #[test]
    fn every_descriptor_agrees_with_its_argument_counts() {
        for c in all() {
            assert!(!c.variants.is_empty(), "{}", c.name);
            let counts: Vec<usize> = c
                .variants
                .iter()
                .map(|v| {
                    let mut inside = false;
                    let mut n = 0;
                    for &w in v.iter() {
                        match w {
                            GROUP_OPEN => {
                                n += 1;
                                inside = true;
                            }
                            GROUP_CLOSE => inside = false,
                            _ if !inside => n += 1,
                            _ => {}
                        }
                    }
                    n
                })
                .collect();
            assert!(*counts.iter().min().unwrap() >= c.min_args as usize, "{}", c.name);
            assert_eq!(*counts.iter().max().unwrap(), c.max_args as usize, "{}", c.name);
            for v in c.variants {
                for &w in *v {
                    assert!(Command::arg_type(w) <= 0x13, "{}", c.name);
                }
            }
        }
    }
}
