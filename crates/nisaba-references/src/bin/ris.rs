//! RIS canonicalization CLI.
//!
//! Reads RIS from stdin, parses it, emits the canonical form to stdout.
//! Used by the TS tools to avoid duplicating authoritative RIS format logic.

use std::io::{self, Read};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let command = args.get(1).map_or("canonical", std::string::String::as_str);

    let mut input = String::new();
    io::stdin()
        .read_to_string(&mut input)
        .expect("failed to read stdin");

    match command {
        "canonical" => {
            let records = nisaba_references::RisRecord::parse(&input).expect("failed to parse RIS");
            let canonical = nisaba_references::RisRecord::write_all(&records)
                .expect("failed to emit canonical RIS");
            print!("{canonical}");
        }
        "count" => {
            let records = nisaba_references::RisRecord::parse(&input).expect("failed to parse RIS");
            println!("{}", records.len());
        }
        _ => {
            eprintln!("unknown command: {command}");
            eprintln!("usage: ris <canonical|count>");
            std::process::exit(1);
        }
    }
}
