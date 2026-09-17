use super::*;

#[test]
fn saves_the_collector_signing_key_without_overwriting_existing_files() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("collector.mac");
    let created = create_collection_account(&path, false)?;
    let account_file = AccountFile::read(&path)?;
    assert_eq!(created.account.id(), account_file.account.id());
    assert!(account_file.account.is_new());
    assert!(account_file.account.is_public());
    assert!(account_file.account.vault().is_empty());
    assert_eq!(account_file.auth_secret_keys.len(), 1);
    assert_eq!(
        account_file.account.storage().get_item(AuthTxFeeCollector::public_key_slot())?,
        miden_protocol::Word::from(account_file.auth_secret_keys[0].public_key().to_commitment()),
    );
    let contents = fs_err::read(&path)?;
    assert!(create_collection_account(&path, false).is_err());
    assert_eq!(fs_err::read(&path)?, contents);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(fs_err::metadata(&path)?.permissions().mode() & 0o777, 0o600);
    }
    Ok(())
}

#[test]
fn clobber_replaces_the_account_file_with_a_new_account() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("collector.mac");
    let previous = create_collection_account(&path, true)?;
    let mut contents = previous.to_bytes();
    contents.extend_from_slice(&[0; 1024]);
    fs_err::write(&path, contents)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs_err::set_permissions(&path, std::fs::Permissions::from_mode(0o644))?;
    }

    let created = create_collection_account(&path, true)?;
    assert_ne!(created.account.id(), previous.account.id());
    assert_eq!(fs_err::read(&path)?, created.to_bytes());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(fs_err::metadata(&path)?.permissions().mode() & 0o777, 0o600);
    }
    Ok(())
}
