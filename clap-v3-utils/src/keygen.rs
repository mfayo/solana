use {
    crate::{
        mnemonic::{language_arg, no_passphrase_arg, word_count_arg},
        ArgConstant,
    },
    clap::{Arg, ArgMatches, Command},
    std::{path::Path, process::exit},
};

pub const NO_OUTFILE_ARG: ArgConstant<'static> = ArgConstant {
    long: "no-outfile",
    name: "no_outfile",
    help: "Only print a seed phrase and pubkey. Do not output a keypair file",
};

pub fn no_outfile_arg<'a>() -> Arg<'a> {
    Arg::new(NO_OUTFILE_ARG.name)
        .long(NO_OUTFILE_ARG.long)
        .help(NO_OUTFILE_ARG.help)
}

pub trait KeyGenerationCommonArgs {
    fn key_generation_common_args(self) -> Self;
}

impl KeyGenerationCommonArgs for Command<'_> {
    fn key_generation_common_args(self) -> Self {
        self.arg(word_count_arg())
            .arg(language_arg())
            .arg(no_passphrase_arg())
    }
}

pub fn check_for_overwrite(outfile: &str, matches: &ArgMatches) {
    let force = matches.is_present("force");
    if !force && Path::new(outfile).exists() {
        eprintln!("Refusing to overwrite {outfile} without --force flag");
        exit(1);
    }
}
