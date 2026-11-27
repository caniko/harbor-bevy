use ed25519_dalek::{Signer, SigningKey};
use p2p_integrity::catalog::{
    AuthenticatedCatalog, CatalogEntry, CatalogError, CatalogVersion, QualificationHandle,
};

const DOMAIN: &[u8] = b"example.registry";
const INITIAL: CatalogVersion = CatalogVersion {
    version: 2,
    sequence: 3,
    epoch: 4,
};
const NEXT: CatalogVersion = CatalogVersion {
    version: 3,
    sequence: 4,
    epoch: 5,
};

#[derive(Debug, PartialEq, Eq)]
struct Entry(&'static [u8], &'static [u8]);

impl CatalogEntry for Entry {
    fn id(&self) -> &[u8] {
        self.0
    }

    fn canonical_payload(&self) -> &[u8] {
        self.1
    }
}

type Catalog = AuthenticatedCatalog<Entry>;

fn signing_key() -> SigningKey {
    SigningKey::from_bytes(&[8; 32])
}

fn signed(stamp: CatalogVersion, entries: Vec<Entry>) -> Catalog {
    let key = signing_key();
    let payload = Catalog::signing_payload(DOMAIN, stamp, &entries).unwrap();
    Catalog::verify(
        DOMAIN,
        stamp,
        entries,
        &key.sign(&payload).to_bytes(),
        &key.verifying_key().to_bytes(),
    )
    .unwrap()
}

#[test]
fn canonical_payload_layout_is_fixed() {
    let entry = Entry(b"ab", b"\0c");
    assert_eq!(
        Catalog::signing_payload(b"d\0", INITIAL, &[entry]).unwrap(),
        b"p2p-integrity.catalog.publication.v1\0\
          \x02\0\0\0d\0\
          \x02\0\0\0\0\0\0\0\x03\0\0\0\0\0\0\0\x04\0\0\0\0\0\0\0\
          \x01\0\0\0\x02\0\0\0ab\x02\0\0\0\0c"
    );
}

#[test]
fn verification_binds_domain_stamp_order_identity_and_payload() {
    let key = signing_key();
    let public_key = key.verifying_key().to_bytes();
    let signature = key
        .sign(&Catalog::signing_payload(DOMAIN, INITIAL, &[Entry(b"a", b"bc")]).unwrap())
        .to_bytes();
    assert_eq!(
        Catalog::verify(
            DOMAIN,
            INITIAL,
            vec![Entry(b"a", b"bc")],
            &signature,
            &public_key
        )
        .unwrap()
        .admit(b"a")
        .unwrap()
        .get(),
        &Entry(b"a", b"bc")
    );
    for (domain, stamp, entry, verify_key) in [
        (b"other".as_slice(), INITIAL, Entry(b"a", b"bc"), public_key),
        (DOMAIN, NEXT, Entry(b"a", b"bc"), public_key),
        (DOMAIN, INITIAL, Entry(b"ab", b"c"), public_key),
        (DOMAIN, INITIAL, Entry(b"a", b"bd"), public_key),
        (
            DOMAIN,
            INITIAL,
            Entry(b"a", b"bc"),
            SigningKey::from_bytes(&[9; 32]).verifying_key().to_bytes(),
        ),
    ] {
        assert_eq!(
            Catalog::verify(domain, stamp, vec![entry], &signature, &verify_key).unwrap_err(),
            CatalogError::InvalidSignature
        );
    }
}

#[test]
fn malformed_catalogs_are_rejected_before_signature_verification() {
    let public_key = signing_key().verifying_key().to_bytes();
    for (entries, error) in [
        (vec![], CatalogError::EmptyCatalog),
        (vec![Entry(b"", b"item")], CatalogError::EmptyEntryId),
        (
            vec![Entry(b"a", b"one"), Entry(b"a", b"two")],
            CatalogError::UnorderedEntries,
        ),
        (
            vec![Entry(b"b", b"one"), Entry(b"a", b"two")],
            CatalogError::UnorderedEntries,
        ),
    ] {
        assert_eq!(
            Catalog::verify(DOMAIN, INITIAL, entries, &[0; 64], &public_key).unwrap_err(),
            error
        );
    }
    assert_eq!(
        Catalog::verify(
            b"",
            INITIAL,
            vec![Entry(b"a", b"item")],
            &[0; 64],
            &public_key
        )
        .unwrap_err(),
        CatalogError::EmptyDomain
    );
    assert_eq!(
        Catalog::verify(
            DOMAIN,
            INITIAL,
            vec![Entry(b"a", b"item")],
            &[255; 64],
            &public_key
        )
        .unwrap_err(),
        CatalogError::InvalidSignature
    );
}

#[test]
fn successor_must_advance_each_counter_and_preserve_every_record() {
    let current = signed(INITIAL, vec![Entry(b"b", b"one"), Entry(b"d", b"two")]);
    let key = signing_key();
    let next = |stamp, entries: Vec<Entry>| {
        let signature = key
            .sign(&Catalog::signing_payload(DOMAIN, stamp, &entries).unwrap())
            .to_bytes();
        current.verify_successor(stamp, entries, &signature)
    };
    for stamp in [
        INITIAL,
        CatalogVersion { version: 1, ..NEXT },
        CatalogVersion {
            version: INITIAL.version,
            ..NEXT
        },
        CatalogVersion {
            sequence: INITIAL.sequence,
            ..NEXT
        },
        CatalogVersion {
            epoch: INITIAL.epoch,
            ..NEXT
        },
    ] {
        assert_eq!(
            next(stamp, vec![Entry(b"b", b"one"), Entry(b"d", b"two")]).unwrap_err(),
            CatalogError::NonIncreasingVersion,
        );
    }
    for entries in [
        vec![Entry(b"b", b"one")],
        vec![Entry(b"b", b"edited"), Entry(b"d", b"two")],
    ] {
        assert_eq!(next(NEXT, entries).unwrap_err(), CatalogError::ChangedEntry);
    }
    let accepted = next(
        NEXT,
        vec![
            Entry(b"b", b"one"),
            Entry(b"c", b"new"),
            Entry(b"d", b"two"),
        ],
    )
    .unwrap();
    assert_eq!(accepted.version(), NEXT);
    assert!(accepted.admit(b"missing").is_none());
    assert!(accepted.admit(b"c").is_some());
    let other_key = SigningKey::from_bytes(&[9; 32]);
    let entries = vec![Entry(b"b", b"one"), Entry(b"d", b"two")];
    let signature = other_key
        .sign(&Catalog::signing_payload(DOMAIN, NEXT, &entries).unwrap())
        .to_bytes();
    assert_eq!(
        current
            .verify_successor(NEXT, entries, &signature)
            .unwrap_err(),
        CatalogError::InvalidSignature
    );
}

#[test]
fn qualification_is_not_publication() {
    let key = signing_key();
    let public_key = key.verifying_key().to_bytes();
    let entry = Entry(b"a", b"candidate");
    let signature = key
        .sign(&QualificationHandle::signing_payload(DOMAIN, &entry).unwrap())
        .to_bytes();
    assert_eq!(
        Catalog::verify(
            DOMAIN,
            INITIAL,
            vec![Entry(b"a", b"candidate")],
            &signature,
            &public_key
        )
        .unwrap_err(),
        CatalogError::InvalidSignature
    );
    let publication = key
        .sign(&Catalog::signing_payload(DOMAIN, INITIAL, &[Entry(b"a", b"candidate")]).unwrap())
        .to_bytes();
    assert_eq!(
        QualificationHandle::verify(DOMAIN, Entry(b"a", b"candidate"), &publication, &public_key)
            .unwrap_err(),
        CatalogError::InvalidSignature
    );
    assert_eq!(
        QualificationHandle::verify(DOMAIN, entry, &signature, &public_key)
            .unwrap()
            .get(),
        &Entry(b"a", b"candidate")
    );
}
