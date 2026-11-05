use super::*;

#[test]
fn a_plain_filename_is_accepted_verbatim() -> Result<(), String> {
    assert_eq!(sanitised_upload_name("notes.md")?, "notes.md");
    assert_eq!(sanitised_upload_name("  notes.md  ")?, "notes.md");
    Ok(())
}

#[test]
fn an_empty_filename_is_rejected() {
    assert!(sanitised_upload_name("").is_err());
    assert!(sanitised_upload_name("   ").is_err());
}

#[test]
fn a_filename_carrying_a_path_segment_is_rejected() {
    assert!(sanitised_upload_name("../escape.md").is_err());
    assert!(sanitised_upload_name("sub/dir.md").is_err());
    assert!(sanitised_upload_name("sub\\dir.md").is_err());
    assert!(sanitised_upload_name(".").is_err());
    assert!(sanitised_upload_name("..").is_err());
}

#[test]
fn known_executable_and_script_extensions_are_denylisted() {
    for filename in ["setup.exe", "install.msi", "run.sh", "payload.js", "malware.scr"] {
        assert!(extension_is_denylisted(filename), "{filename} should be denylisted");
    }
}

#[test]
fn extension_denylist_is_case_insensitive() {
    assert!(extension_is_denylisted("SETUP.EXE"));
}

#[test]
fn archives_and_documents_are_not_denylisted_by_extension() {
    for filename in [
        "report.docx",
        "sheet.xlsx",
        "slides.pptx",
        "photos.zip",
        "scan.pdf",
        "photo.png",
    ] {
        assert!(
            !extension_is_denylisted(filename),
            "{filename} should not be denylisted"
        );
    }
}

#[test]
fn a_filename_with_no_extension_is_not_denylisted_by_extension_alone() {
    assert!(!extension_is_denylisted("README"));
}

#[test]
fn elf_magic_bytes_are_content_denylisted_regardless_of_extension() {
    // `infer`'s own ELF matcher requires more than 52 bytes on top of the magic number, so this
    // pads with zeroes past that floor rather than asserting on the magic bytes alone.
    let mut elf_header = vec![0x7F, b'E', b'L', b'F', 0x02, 0x01, 0x01, 0x00];
    elf_header.resize(64, 0);
    assert!(content_is_denylisted_executable(&elf_header));
}

#[test]
fn plain_and_pdf_content_is_not_content_denylisted() {
    assert!(!content_is_denylisted_executable(b"just some plain text"));
    assert!(!content_is_denylisted_executable(b"%PDF-1.4\n%\xE2\xE3\xCF\xD3\n"));
}

#[test]
fn a_pem_certificate_is_not_content_denylisted() {
    // A regression case: `infer` classifies PEM/DER content under the same `MatcherType::App`
    // category as a real executable, but a PEM-armoured certificate or key is plain text an
    // operator can plausibly drop in the Inbox, not executable content.
    let pem = b"-----BEGIN CERTIFICATE-----\nMIIB...==\n-----END CERTIFICATE-----\n";
    assert!(!content_is_denylisted_executable(pem));
}

#[test]
fn generic_asn1_der_content_is_not_content_denylisted() {
    let der: &[u8] = &[0x30, 0x82, 0x01, 0x02, 0x03];
    assert!(!content_is_denylisted_executable(der));
}

#[test]
fn a_windows_pe_header_is_content_denylisted_regardless_of_extension() {
    let pe_header: &[u8] = &[b'M', b'Z', 0x90, 0x00, 0x03, 0x00, 0x00, 0x00];
    assert!(content_is_denylisted_executable(pe_header));
}

#[test]
fn a_zip_archive_is_not_content_denylisted() {
    // The ZIP local file header signature alone (no embedded executable), the same shape a real
    // `.zip`/`.docx`/`.xlsx`/`.pptx` upload's own leading bytes carry: archives are accepted, so
    // this must never be treated as an executable.
    let zip_header: &[u8] = &[0x50, 0x4B, 0x03, 0x04, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    assert!(!content_is_denylisted_executable(zip_header));
}

#[test]
fn the_upload_size_limit_admits_exactly_the_limit_and_refuses_one_byte_more() {
    // `fs_write_file` decodes base64 to the exact length, so the boundary compares the raw byte
    // count alone, whichever write path the content then takes.
    let limit = usize::try_from(MAX_UPLOAD_BYTES).unwrap_or(usize::MAX);
    assert_eq!(upload_boundary_rejection("exact.dat", &vec![0x80_u8; limit]), None);
    assert!(upload_boundary_rejection("over.dat", &vec![0x80_u8; limit + 1]).is_some());
}

#[test]
fn an_oversized_denylisted_extension_is_rejected_for_its_size_not_its_type() -> Result<(), String> {
    // Locks in the check ordering: the size check runs first, so a file that is both oversized
    // and a denylisted type gets the plain "10 MB" message, not the executable-rejection
    // message. If the two checks were ever swapped, this would fail.
    let oversized = vec![b'a'; usize::try_from(MAX_UPLOAD_BYTES).unwrap_or(usize::MAX - 1) + 1];
    let reason = upload_boundary_rejection("install.exe", &oversized).ok_or("oversized file must be rejected")?;
    assert!(reason.contains("10 MB"), "{reason}");
    assert!(!reason.contains("executable"), "{reason}");
    Ok(())
}

#[test]
fn a_denylisted_extension_under_the_size_cap_is_rejected_for_its_type() -> Result<(), String> {
    let reason =
        upload_boundary_rejection("install.exe", b"small payload").ok_or("denylisted file must be rejected")?;
    assert!(reason.contains("executable"), "{reason}");
    Ok(())
}

#[test]
fn an_accepted_file_under_the_cap_has_no_boundary_rejection() {
    assert!(upload_boundary_rejection("notes.md", b"# Notes\n").is_none());
}
