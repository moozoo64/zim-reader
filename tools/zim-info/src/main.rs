use std::process::ExitCode;

use zim_reader::{Archive, ArchiveOptions, VerifyChecksum};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let path = match args.as_slice() {
        [p] => p.clone(),
        _ => {
            eprintln!("Usage: zim-info <archive.zim>");
            return ExitCode::from(2);
        }
    };

    let mut opts = ArchiveOptions::default();
    opts.verify_checksum = VerifyChecksum::Skip;

    let archive = match Archive::open_with_options(&path, opts) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("zim-info: {e}");
            return ExitCode::from(1);
        }
    };

    let h = archive.header();
    println!("path:            {path}");
    println!("magic_number:    0x{:08X}", h.magic_number);
    println!("version:         {}.{}", h.major_version, h.minor_version);
    println!("uuid:            {}", format_uuid(&h.uuid));
    println!("entry_count:     {}", h.entry_count);
    println!("cluster_count:   {}", h.cluster_count);
    println!("path_ptr_pos:    {}", h.path_ptr_pos);
    println!("title_ptr_pos:   {}", h.title_ptr_pos);
    println!("cluster_ptr_pos: {}", h.cluster_ptr_pos);
    println!("mime_list_pos:   {}", h.mime_list_pos);
    println!(
        "main_page:       {}",
        h.main_page
            .map(|i| i.to_string())
            .unwrap_or_else(|| "absent".into())
    );
    println!(
        "layout_page:     {}",
        h.layout_page
            .map(|i| i.to_string())
            .unwrap_or_else(|| "absent".into())
    );
    println!("checksum_pos:    {}", h.checksum_pos);

    let mimes = archive.mime_types();
    println!();
    println!("MIME types ({}):", mimes.len());
    for (i, m) in mimes.iter().enumerate() {
        println!("  {i:4}  {m}");
    }

    ExitCode::SUCCESS
}

fn format_uuid(uuid: &[u8; 16]) -> String {
    let hex: String = uuid.iter().map(|b| format!("{b:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}
