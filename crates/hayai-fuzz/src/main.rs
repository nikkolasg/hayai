//! `hayai-fuzz`: long runs of the differential fuzzer, and the replay of one case.

use std::path::PathBuf;
use std::process::ExitCode;

use hayai_fuzz::classes::Class;
use hayai_fuzz::header;
use hayai_fuzz::run::{self, Finding, Plan};

const USAGE: &str = "\
hayai-fuzz [--seed N] [--iterations N] [--seconds N] [--class NAME]... [--out DIR]
           [--per-signature N]
hayai-fuzz --class NAME --case-seed N
hayai-fuzz --replay FILE

  --seed N           seed of the run (default 1)
  --iterations N     cases for each class
  --seconds N        wall time for each class (default 60 when no --iterations)
  --class NAME       a class to run; more than one is possible (default: all)
  --out DIR          directory of the case files (default target/fuzz/findings)
  --per-signature N  findings with one signature to minimise and write (default 3)
  --case-seed N      run the case with this case seed and print it
  --replay FILE      run the recipe of a case file and print the two verdicts

Classes: header structure coinbase height txfields script spend shielded limits
commitments bytes header-context pow
The run uses all cores. The exit code is 1 when the run has a finding.";

fn number(text: &str) -> Result<u64, String> {
    let parsed = match text.strip_prefix("0x") {
        Some(hex) => u64::from_str_radix(hex, 16),
        None => text.parse(),
    };
    parsed.map_err(|e| format!("{text}: {e}"))
}

struct Args {
    plan: Plan,
    header_classes: Vec<header::HeaderClass>,
    case_seed: Option<u64>,
    replay: Option<PathBuf>,
}

fn parse_args() -> Result<Args, String> {
    let mut plan = Plan {
        seed: 1,
        classes: Vec::new(),
        iterations: None,
        seconds: None,
        out_dir: Some(run::default_out_dir()),
        per_signature: 3,
    };
    let mut header_classes = Vec::new();
    let mut named = false;
    let mut case_seed = None;
    let mut replay = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = || args.next().ok_or(format!("{arg} needs a value"));
        match arg.as_str() {
            "--seed" => plan.seed = number(&value()?)?,
            "--iterations" => plan.iterations = Some(number(&value()?)?),
            "--seconds" => plan.seconds = Some(number(&value()?)?),
            "--per-signature" => plan.per_signature = number(&value()?)? as usize,
            "--out" => plan.out_dir = Some(PathBuf::from(value()?)),
            "--case-seed" => case_seed = Some(number(&value()?)?),
            "--replay" => replay = Some(PathBuf::from(value()?)),
            "--class" => {
                let name = value()?;
                named = true;
                match (
                    Class::from_name(&name),
                    header::HeaderClass::from_name(&name),
                ) {
                    (Some(class), _) => plan.classes.push(class),
                    (None, Some(class)) => header_classes.push(class),
                    (None, None) => return Err(format!("no class {name}")),
                }
            }
            "--help" | "-h" => return Err(String::new()),
            other => return Err(format!("unknown argument {other}")),
        }
    }
    if !named {
        plan.classes = Class::ALL.to_vec();
        header_classes = header::HeaderClass::ALL.to_vec();
    }
    if let (None, None) = (plan.iterations, plan.seconds) {
        plan.seconds = Some(60);
    }
    Ok(Args {
        plan,
        header_classes,
        case_seed,
        replay,
    })
}

fn print_finding(finding: &Finding) {
    println!(
        "{}",
        serde_json::to_string_pretty(finding).expect("a finding is JSON")
    );
}

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(args) => args,
        Err(message) => {
            if !message.is_empty() {
                eprintln!("error: {message}\n");
            }
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    run::init();

    if let Some(path) = &args.replay {
        let text = std::fs::read_to_string(path).expect("read the case file");
        let finding: Finding = serde_json::from_str(&text).expect("the file is a case file");
        let Some(recipe) = &finding.recipe else {
            eprintln!("the case file has no recipe: run its class with --case-seed");
            return ExitCode::from(2);
        };
        let outcome = run::check_recipe(recipe).expect("the seed of the recipe exists");
        println!(
            "{}",
            serde_json::to_string_pretty(&outcome).expect("an outcome is JSON")
        );
        return ExitCode::SUCCESS;
    }
    if let Some(case_seed) = args.case_seed {
        match (args.plan.classes.first(), args.header_classes.first()) {
            (Some(&class), _) => {
                let recipe = run::recipe_of(class, case_seed);
                let outcome = run::check_recipe(&recipe).expect("the seed of the recipe exists");
                println!(
                    "{}\n{}",
                    serde_json::to_string_pretty(&recipe).expect("a recipe is JSON"),
                    serde_json::to_string_pretty(&outcome).expect("an outcome is JSON")
                );
            }
            (None, Some(&class)) => {
                let case = header::case(class, case_seed);
                println!("{case:#?}\n{:#?}", header::check(&case));
            }
            (None, None) => unreachable!("a run without --class has every class"),
        }
        return ExitCode::SUCCESS;
    }

    let mut report = run::run(&args.plan);
    header::run(&args.plan, &args.header_classes, &mut report);
    for (class, stats) in &report.classes {
        println!(
            "{class}: {} cases in {:.1} s, {} findings, known {:?}",
            stats.cases, stats.seconds, stats.findings, stats.known
        );
        for (comparison, count) in &stats.comparisons {
            println!("    {comparison:?}: {count}");
        }
        for (pair, count) in &stats.pairs {
            println!("        {pair}: {count}");
        }
    }
    for finding in &report.findings {
        print_finding(finding);
    }
    if let Some(dir) = &args.plan.out_dir {
        std::fs::create_dir_all(dir).expect("create the output directory");
        let path = dir.join(format!("report-{:016x}.json", report.seed));
        let json = serde_json::to_string_pretty(&report).expect("a report is JSON");
        std::fs::write(&path, json).expect("write the report");
        println!("report: {}", path.display());
    }
    if report.findings.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
