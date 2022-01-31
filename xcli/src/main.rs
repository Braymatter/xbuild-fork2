use anyhow::Result;
use clap::{Parser, Subcommand};
use std::fs::File;
use std::path::{Path, PathBuf};
use xcli::{Config, Format, Mode};
use xcommon::{Signer, ZipFileOptions};

#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
struct Args {
    #[clap(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    Build {
        #[clap(flatten)]
        build: BuildOptions,
        #[clap(flatten)]
        sign: SignOptions,
    },
    Sign {
        #[clap(flatten)]
        sign: SignOptions,
        file: PathBuf,
    },
    Verify {
        file: PathBuf,
    },
    Run {
        #[clap(flatten)]
        build: BuildOptions,
        #[clap(flatten)]
        sign: SignOptions,
        #[clap(flatten)]
        run: RunOptions,
    },
    Devices,
}

#[derive(Parser, Debug)]
struct BuildOptions {
    #[clap(long)]
    debug: bool,
    #[clap(long)]
    target: Option<String>,
}

#[derive(Parser, Debug)]
struct SignOptions {
    #[clap(long)]
    key: Option<PathBuf>,
    #[clap(long)]
    cert: Option<PathBuf>,
}

impl SignOptions {
    fn signer(&self) -> Result<Option<Signer>> {
        if let (Some(key), Some(cert)) = (self.key.as_ref(), self.cert.as_ref()) {
            let key = std::fs::read_to_string(key)?;
            let cert = std::fs::read_to_string(cert)?;
            Ok(Some(Signer::new(&key, &cert)?))
        } else {
            Ok(None)
        }
    }
}

#[derive(Parser, Debug)]
struct RunOptions {
    #[clap(short, long)]
    device: String,
}

fn main() -> Result<()> {
    let args = Args::parse();
    match args.command {
        Commands::Build { build, sign } => {
            let path = cmd_build_and_sign(build, sign)?;
            println!("built {}", path.display());
        }
        Commands::Sign { sign, file } => {
            cmd_sign(sign, &file)?;
        }
        Commands::Verify { file } => {
            cmd_verify(&file)?;
        }
        Commands::Run { build, sign, run } => {
            cmd_run(build, sign, run)?;
        }
        Commands::Devices => {
            cmd_devices()?;
        }
    }
    Ok(())
}

fn cmd_build_and_sign(build: BuildOptions, sign: SignOptions) -> Result<PathBuf> {
    let format = if let Some(triple) = build.target.as_deref() {
        Format::from_target(triple)?
    } else {
        Format::from_target(xcli::host_triple()?)?
    };
    let signer = sign.signer()?;
    let (config, mode) = if Path::new("Cargo.toml").exists() {
        (Config::parse("Cargo.toml")?, Mode::Cargo)
    } else if Path::new("pubspec.yaml").exists() {
        (Config::parse("pubspec.yaml")?, Mode::Flutter)
    } else {
        anyhow::bail!("config file not found");
    };
    let opt = if build.debug { "debug" } else { "release" };
    let out_dir = match mode {
        Mode::Cargo => Path::new("target").join(opt),
        Mode::Flutter => Path::new("build").join(opt),
    };
    std::fs::create_dir_all(&out_dir)?;
    match (mode, format) {
        (Mode::Flutter, Format::Appimage) => {
            xcli::flutter_build("linux", build.debug)?;
            let out = out_dir.join(format!("{}-x86_64.AppImage", &config.name));
            let build_dir = Path::new("build").join("linux").join("x64").join(opt);
            let builder = xappimage::AppImageBuilder::new(&build_dir, &out, config.name.clone())?;
            builder.add_directory(&build_dir.join("bundle"), None)?;
            builder.add_apprun()?;
            builder.add_desktop()?;
            if let Some(icon) = config.icon(Format::Appimage) {
                builder.add_icon(icon)?;
            }
            builder.sign(signer)?;
            Ok(out)
        }
        (Mode::Flutter, Format::Apk) => {
            xcli::flutter_build("apk", build.debug)?;
            let out = out_dir.join(format!("{}-aarch64.apk", &config.name));
            let mut apk = File::create(&out)?;
            let mut builder = xapk::ApkBuilder::new(&mut apk);
            let intermediates = Path::new("build").join("app").join("intermediates");
            let assets = intermediates.join("merged_assets").join(opt).join("out");
            builder.add_directory(&assets, Some(Path::new("assets")))?;
            let libs = intermediates
                .join("merged_native_libs")
                .join(opt)
                .join("out");
            builder.add_directory(&libs, None)?;
            let dex = if build.debug {
                "mergeDexDebug"
            } else {
                "minifyReleaseWithR8"
            };
            let classes = intermediates
                .join("dex")
                .join(opt)
                .join(dex)
                .join("classes.dex");
            builder.add_file(&classes, "classes.dex", ZipFileOptions::Compressed)?;
            /*let manifest = intermediates
            .join("merged_manifest")
            .join(opt)
            .join("out")
            .join("AndroidManifest.xml");*/
            let manifest = Path::new("android")
                .join("app")
                .join("src")
                .join("main")
                .join("AndroidManifest.xml");
            builder.add_manifest(&xapk::Xml::from_path(&manifest)?)?;
            builder.build()?;
            xapk::sign::sign(&out, signer)?;
            Ok(out)
        }
        f => unimplemented!("{:?}", f),
    }
}

fn cmd_sign(opts: SignOptions, file: &Path) -> Result<()> {
    match Format::from_path(file)? {
        Format::Apk => xapk::sign::sign(file, opts.signer()?)?,
        f => unimplemented!("{:?}", f),
    }
    Ok(())
}

fn cmd_verify(file: &Path) -> Result<()> {
    let certs = match Format::from_path(file)? {
        Format::Apk => xapk::sign::verify(file)?,
        Format::Msix => {
            let signed_data = xmsix::p7x::read_p7x(file)?;
            for signer in &signed_data.signer_infos {
                if let rasn_cms::SignerIdentifier::IssuerAndSerialNumber(isn) = &signer.sid {
                    println!("issuer: {}", xcli::display_cert_name(&isn.issuer)?);
                }
            }
            return Ok(());
        }
        f => unimplemented!("{:?}", f),
    };
    for cert in certs {
        println!(
            "subject: {}",
            xcli::display_cert_name(&cert.tbs_certificate.subject)?
        );
        println!(
            "issuer: {}",
            xcli::display_cert_name(&cert.tbs_certificate.issuer)?
        );
    }
    Ok(())
}

fn cmd_run(build: BuildOptions, sign: SignOptions, opts: RunOptions) -> Result<()> {
    //let path = cmd_build_and_sign(build, sign)?;
    let adb = xcli::adb::Adb::which()?;
    //adb.install(&opts.device, file)?;
    adb.flutter_attach(&opts.device, "com.example.helloworld", ".MainActivity")?;
    Ok(())
}

fn cmd_devices() -> Result<()> {
    if let Ok(adb) = xcli::adb::Adb::which() {
        for device in adb.devices()? {
            println!("{}", device);
        }
    }
    Ok(())
}