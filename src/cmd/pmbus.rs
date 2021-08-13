/*
 * Copyright 2020 Oxide Computer Company
 */

use crate::cmd::{Archive, Attach, Validate};
use crate::core::Core;
use crate::hiffy::*;
use crate::hubris::*;
use crate::Args;
use std::convert::TryFrom;
use std::thread;

use anyhow::{anyhow, bail, Context, Result};
use hif::*;
use std::collections::HashMap;
use std::time::Duration;
use structopt::clap::App;
use structopt::StructOpt;

#[derive(StructOpt, Debug)]
#[structopt(name = "pmbus", about = "scan for and read PMBus devices")]
struct PmbusArgs {
    /// sets timeout
    #[structopt(
        long, short, default_value = "5000", value_name = "timeout_ms",
        parse(try_from_str = parse_int::parse)
    )]
    timeout: u32,

    /// verbose output
    #[structopt(long, short)]
    verbose: bool,

    /// show errors
    #[structopt(long, short)]
    errors: bool,

    /// specifies a PMBus driver
    #[structopt(long, short = "D")]
    driver: Option<String>,

    /// specifies an I2C controller
    #[structopt(long, short, value_name = "controller",
        parse(try_from_str = parse_int::parse),
    )]
    controller: u8,

    /// specifies an I2C controller port
    #[structopt(long, short, value_name = "port")]
    port: Option<String>,

    /// specifies I2C multiplexer and segment
    #[structopt(long, short, value_name = "mux:segment")]
    mux: Option<String>,

    /// specifies an I2C device address
    #[structopt(long, short = "d", value_name = "address",
        parse(try_from_str = parse_int::parse),
    )]
    device: u8,
}

fn pmbus_result(
    subargs: &PmbusArgs,
    device: pmbus::Device,
    code: u8,
    command: &dyn pmbus::Command,
    result: &Result<Vec<u8>, u32>,
    errmap: &HashMap<u32, String>,
) -> Result<()> {
    let nbytes = match command.read_op() {
        pmbus::Operation::ReadByte => Some(1),
        pmbus::Operation::ReadWord => Some(2),
        pmbus::Operation::ReadBlock => None,
        _ => {
            unreachable!();
        }
    };

    let name = format!("{:?}", command);
    let cmdstr = format!("0x{:02x} {:<25}", code, name);

    match result {
        Err(err) => {
            if subargs.errors {
                println!("{} Err({})", cmdstr, errmap.get(err).unwrap());
            }
        }

        Ok(val) => {
            if val.len() == 0 {
                if subargs.errors {
                    println!("{} Timed out", cmdstr);
                    return Ok(());
                }
            }

            let n = match nbytes {
                Some(1) => {
                    println!("{} 0x{:02x}", cmdstr, val[0]);
                    1
                }

                Some(2) => {
                    if val.len() > 1 {
                        let word = ((val[1] as u16) << 8) | (val[0] as u16);
                        println!("{} 0x{:04x}", cmdstr, word);
                    } else {
                        println!("{} Short: {:?}", cmdstr, val);
                    }
                    2
                }

                Some(_) => {
                    unreachable!();
                }

                None => {
                    print!("{}", cmdstr);
                    for i in 0..val.len() {
                        print!(" 0x{:02x}", val[i]);
                    }
                    println!();
                    val.len()
                }
            };

            if !subargs.verbose {
                return Ok(());
            }

            let mut printed = false;

            let _ = device.interpret(code, &val[0..n], |field, value| {
                let (pos, width) = field.bits();

                let bits = if width.0 == 1 {
                    format!("b{}", pos.0)
                } else {
                    format!("b{}:{}", pos.0 + width.0 - 1, pos.0)
                };

                let value = format!("{}", value);

                println!("     | {:6} {:<30} <= {}", bits, value, field.name());
                printed = true;
            });

            if printed {
                println!(
                    "     +------------------------------------------\
                    -----------------------------\n"
                );
            }
        }
    }

    Ok(())
}

fn pmbus(
    hubris: &mut HubrisArchive,
    core: &mut dyn Core,
    _args: &Args,
    subargs: &Vec<String>,
) -> Result<()> {
    let subargs = PmbusArgs::from_iter_safe(subargs)?;

    let mut context = HiffyContext::new(hubris, core, subargs.timeout)?;
    let funcs = context.functions()?;
    let func = funcs
        .get("I2cRead")
        .ok_or_else(|| anyhow!("did not find I2cRead function"))?;

    if func.args.len() != 7 {
        bail!("mismatched function signature on I2cRead");
    }

    let mut port = None;

    if let Some(ref portarg) = subargs.port {
        let p = hubris
            .lookup_enum(func.args[1])
            .context("expected port to be an enum")?;

        if p.size != 1 {
            bail!("expected port to be a 1-byte enum");
        }

        for variant in &p.variants {
            if variant.name.eq_ignore_ascii_case(&portarg) {
                port = Some(u8::try_from(variant.tag.unwrap())?);
                break;
            }
        }

        if port.is_none() {
            let mut vals: Vec<String> = vec![];

            for variant in &p.variants {
                vals.push(variant.name.to_string());
            }

            bail!(
                "invalid port \"{}\" (must be one of: {})",
                portarg,
                vals.join(", ")
            );
        }
    }

    let mux = if let Some(mux) = &subargs.mux {
        let s = mux
            .split(":")
            .map(|v| parse_int::parse::<u8>(v))
            .collect::<Result<Vec<_>, _>>()
            .context("expected multiplexer and segment to be integers")?;

        if s.len() == 2 {
            Some((s[0], s[1]))
        } else if s.len() == 1 {
            Some((0, s[0]))
        } else {
            bail!("expected only multiplexer and segment identifiers");
        }
    } else {
        None
    };

    let device = if let Some(driver) = &subargs.driver {
        match pmbus::Device::from_str(driver) {
            Some(device) => device,
            None => {
                bail!("unknown device \"{}\"", driver);
            }
        }
    } else {
        pmbus::Device::Common
    };

    let mut ops = vec![];
    let mut cmds = vec![];

    ops.push(Op::Push(subargs.controller));

    if let Some(port) = port {
        ops.push(Op::Push(port));
    } else {
        ops.push(Op::PushNone);
    }

    if let Some(mux) = mux {
        ops.push(Op::Push(mux.0));
        ops.push(Op::Push(mux.1));
    } else {
        ops.push(Op::PushNone);
        ops.push(Op::PushNone);
    }

    ops.push(Op::Push(subargs.device));

    for i in 0..=255u8 {
        device.command(i, |cmd| {
            let op = match cmd.read_op() {
                pmbus::Operation::ReadByte => Op::Push(1),
                pmbus::Operation::ReadWord => Op::Push(2),
                pmbus::Operation::ReadBlock => Op::PushNone,
                _ => {
                    return;
                }
            };

            ops.push(Op::Push(i));
            ops.push(op);
            ops.push(Op::Call(func.id));
            ops.push(Op::Drop);
            ops.push(Op::Drop);
            cmds.push(i);
        });
    }

    ops.push(Op::Done);

    context.execute(core, ops.as_slice())?;

    loop {
        if context.done(core)? {
            break;
        }

        thread::sleep(Duration::from_millis(100));
    }

    let results = context.results(core)?;

    for i in 0..results.len() {
        let mut r = Ok(());

        device.command(cmds[i], |cmd| {
            r = pmbus_result(
                &subargs,
                device,
                cmds[i],
                cmd,
                &results[i],
                &func.errmap,
            );
        });

        r?;
    }

    Ok(())
}

pub fn init<'a, 'b>() -> (crate::cmd::Command, App<'a, 'b>) {
    (
        crate::cmd::Command::Attached {
            name: "pmbus",
            archive: Archive::Required,
            attach: Attach::LiveOnly,
            validate: Validate::Booted,
            run: pmbus,
        },
        PmbusArgs::clap(),
    )
}