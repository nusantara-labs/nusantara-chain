use crate::config::config_dir;
use crate::error::CliError;
use crate::keypair;

pub fn run(outfile: Option<String>) -> Result<(), CliError> {
    let kp = keypair::generate_keypair();
    let address = kp.address().to_base64();

    let path = outfile.unwrap_or_else(|| {
        config_dir().join("id.key").to_string_lossy().to_string()
    });

    keypair::save_keypair(&path, &kp)?;
    println!("Keypair generated:");
    println!("  Address: {address}");
    println!("  Saved to: {path}");
    Ok(())
}
