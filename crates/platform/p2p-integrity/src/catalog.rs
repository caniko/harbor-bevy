//! Signed, append-only catalogs with separate qualification and admission tokens.
//!
//! Callers must configure trusted keys and a domain out of band, and bound
//! input sizes before decoding an untrusted catalog.

use ed25519_dalek::{Signature, VerifyingKey};
use thiserror::Error;

const PUBLICATION_TAG: &[u8] = b"p2p-integrity.catalog.publication.v1\0";
const QUALIFICATION_TAG: &[u8] = b"p2p-integrity.catalog.qualification.v1\0";

/// Maximum canonical message size for publication and qualification signatures.
pub const MAX_SIGNING_PAYLOAD_BYTES: usize = 16 * 1024 * 1024;

/// Application-defined immutable data authenticated by a catalog signature.
pub trait CatalogEntry {
    /// Stable, nonempty identifier. Catalogs sort IDs by byte order.
    fn id(&self) -> &[u8];

    /// Deterministic, unambiguous encoding of all admission-relevant fields
    /// other than the separately signed ID. The ID, bytes, and admitted data
    /// must not change while the entry is held by a catalog or handle.
    fn canonical_payload(&self) -> &[u8];
}

/// Independent counters. Each must strictly increase on every successor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CatalogVersion {
    pub version: u64,
    pub sequence: u64,
    pub epoch: u64,
}

/// An invalid catalog, signature, or transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum CatalogError {
    #[error("catalog domain is empty")]
    EmptyDomain,
    #[error("catalog domain must be UTF-8")]
    InvalidDomainEncoding,
    #[error("catalog contains no entries")]
    EmptyCatalog,
    #[error("catalog entry ID is empty")]
    EmptyEntryId,
    #[error("catalog entry IDs must be unique and sorted")]
    UnorderedEntries,
    #[error("field length or entry count exceeds u32")]
    EncodingTooLarge,
    #[error("catalog signing payload exceeds the byte limit or could not be allocated")]
    PayloadTooLarge,
    #[error("catalog signature verification failed")]
    InvalidSignature,
    #[error("handle does not match the expected key, domain, or catalog version")]
    AuthorityMismatch,
    #[error("successor must advance the catalog version, sequence, and epoch")]
    NonIncreasingVersion,
    #[error("successor removed or changed a previously authenticated entry")]
    ChangedEntry,
}

/// A snapshot authenticated under a caller-configured publication key and domain.
///
/// Bootstrap with [`Self::verify`]; to prevent rollback, retain the latest
/// trusted snapshot across restarts and use [`Self::verify_successor`] for updates.
#[derive(Debug)]
pub struct AuthenticatedCatalog<E> {
    domain: String,
    version: CatalogVersion,
    entries: Vec<E>,
    publication_key: VerifyingKey,
}

impl<E: CatalogEntry> AuthenticatedCatalog<E> {
    /// Build the exact Ed25519 message: fixed tag, length-prefixed UTF-8 domain,
    /// three LE `u64` counters, LE `u32` count, then each length-prefixed
    /// ID and payload in sorted ID order. Length prefixes are LE `u32`.
    pub fn signing_payload(
        domain: &[u8],
        version: CatalogVersion,
        entries: &[E],
    ) -> Result<Vec<u8>, CatalogError> {
        Self::signing_payload_with_limit(domain, version, entries, MAX_SIGNING_PAYLOAD_BYTES)
    }

    fn signing_payload_with_limit(
        domain: &[u8],
        version: CatalogVersion,
        entries: &[E],
        limit: usize,
    ) -> Result<Vec<u8>, CatalogError> {
        let mut len = header_len(PUBLICATION_TAG, domain, limit)?;
        if entries.is_empty() {
            return Err(CatalogError::EmptyCatalog);
        }
        let count = encode_len(entries.len())?;
        len = checked_len(len, size_of::<[u64; 3]>() + count.len(), limit)?;
        let mut previous_id = None;
        for entry in entries {
            let id = entry.id();
            if previous_id.is_some_and(|previous| previous >= id) {
                return Err(CatalogError::UnorderedEntries);
            }
            len = entry_end(len, id, entry.canonical_payload(), limit)?;
            previous_id = Some(id);
        }

        let mut out = start_payload(PUBLICATION_TAG, domain, len)?;
        for counter in [version.version, version.sequence, version.epoch] {
            out.extend_from_slice(&counter.to_le_bytes());
        }
        out.extend_from_slice(&count);
        for entry in entries {
            write_entry(&mut out, entry.id(), entry.canonical_payload(), len)?;
        }
        Ok(out)
    }

    /// Verify a snapshot against an externally trusted publication key.
    pub fn verify(
        domain: &[u8],
        version: CatalogVersion,
        entries: Vec<E>,
        signature: &[u8; 64],
        publication_key: &[u8; 32],
    ) -> Result<Self, CatalogError> {
        let payload = Self::signing_payload(domain, version, &entries)?;
        let publication_key = verify_signature(&payload, signature, publication_key)?;
        Ok(Self {
            domain: domain_text(domain)?.to_owned(),
            version,
            entries,
            publication_key,
        })
    }

    /// Verify a successor with the same domain and key. All previously signed
    /// entries must still be present and byte-identical; new IDs may appear anywhere.
    pub fn verify_successor(
        &self,
        version: CatalogVersion,
        entries: Vec<E>,
        signature: &[u8; 64],
    ) -> Result<Self, CatalogError> {
        if version.version <= self.version.version
            || version.sequence <= self.version.sequence
            || version.epoch <= self.version.epoch
        {
            return Err(CatalogError::NonIncreasingVersion);
        }
        let next = Self::verify(
            self.domain.as_bytes(),
            version,
            entries,
            signature,
            self.publication_key.as_bytes(),
        )?;
        let mut next_entries = next.entries.iter();
        if !self.entries.iter().all(|previous| {
            next_entries.any(|current| {
                current.id() == previous.id()
                    && current.canonical_payload() == previous.canonical_payload()
            })
        }) {
            return Err(CatalogError::ChangedEntry);
        }
        Ok(next)
    }

    /// Return the authenticated snapshot counters.
    pub const fn version(&self) -> CatalogVersion {
        self.version
    }

    /// Admit the exact authenticated entry for this ID, not a separately supplied value.
    pub fn admit(&self, id: &[u8]) -> Option<AdmittedHandle<&E>> {
        let index = self
            .entries
            .binary_search_by(|entry| entry.id().cmp(id))
            .ok()?;
        Some(AdmittedHandle {
            entry: self.entries.get(index)?,
            verifying_key: self.publication_key,
            domain: self.domain.clone(),
            catalog_version: self.version,
        })
    }

    /// Borrow an admitted value only if its key, domain, and version match.
    pub fn verify_admitted<'a, T>(
        &self,
        handle: &'a AdmittedHandle<T>,
    ) -> Result<&'a T, CatalogError> {
        if handle.catalog_version() != self.version {
            return Err(CatalogError::AuthorityMismatch);
        }
        handle.require_authority(&self.publication_key, &self.domain)
    }
}

/// A candidate authenticated under a qualification key, not an admission token.
///
/// ```compile_fail,E0308
/// use p2p_integrity::catalog::{AdmittedHandle, QualificationHandle};
///
/// fn publish(candidate: QualificationHandle<u8>) {
///     let _: AdmittedHandle<u8> = candidate;
/// }
/// ```
#[derive(Debug)]
pub struct QualificationHandle<T> {
    candidate: T,
    verifying_key: VerifyingKey,
    domain: String,
}

impl<T: CatalogEntry> QualificationHandle<T> {
    /// Build the Ed25519 message using the qualification tag, then the same
    /// length-prefixed domain, ID, and payload encoding as publication.
    pub fn signing_payload(domain: &[u8], candidate: &T) -> Result<Vec<u8>, CatalogError> {
        let limit = MAX_SIGNING_PAYLOAD_BYTES;
        let (id, payload) = (candidate.id(), candidate.canonical_payload());
        let len = entry_end(
            header_len(QUALIFICATION_TAG, domain, limit)?,
            id,
            payload,
            limit,
        )?;
        let mut out = start_payload(QUALIFICATION_TAG, domain, len)?;
        write_entry(&mut out, id, payload, len)?;
        Ok(out)
    }

    /// Authenticate a candidate under an externally trusted qualification key.
    pub fn verify(
        domain: &[u8],
        candidate: T,
        signature: &[u8; 64],
        qualification_key: &[u8; 32],
    ) -> Result<Self, CatalogError> {
        let payload = Self::signing_payload(domain, &candidate)?;
        let verifying_key = verify_signature(&payload, signature, qualification_key)?;
        Ok(Self {
            candidate,
            verifying_key,
            domain: domain_text(domain)?.to_owned(),
        })
    }
}

impl<T> QualificationHandle<T> {
    /// Borrow the qualified candidate without granting admission.
    pub const fn get(&self) -> &T {
        &self.candidate
    }

    /// Return the trusted qualification key used to verify this candidate.
    pub const fn verifying_key(&self) -> &VerifyingKey {
        &self.verifying_key
    }

    /// Return the signed qualification domain.
    pub fn domain(&self) -> &str {
        &self.domain
    }

    /// Borrow the candidate only if its qualification key and domain match.
    pub fn require_authority(
        &self,
        expected_key: &VerifyingKey,
        expected_domain: &str,
    ) -> Result<&T, CatalogError> {
        if self.verifying_key() != expected_key || self.domain() != expected_domain {
            return Err(CatalogError::AuthorityMismatch);
        }
        Ok(self.get())
    }
}

/// A token proving that an entry is present in an authenticated catalog.
#[derive(Debug)]
pub struct AdmittedHandle<T> {
    entry: T,
    verifying_key: VerifyingKey,
    domain: String,
    catalog_version: CatalogVersion,
}

impl<T> AdmittedHandle<T> {
    /// Return the trusted publication key used to verify the catalog.
    pub const fn verifying_key(&self) -> &VerifyingKey {
        &self.verifying_key
    }

    /// Return the signed publication domain.
    pub fn domain(&self) -> &str {
        &self.domain
    }

    /// Return all three authenticated counters of the issuing catalog.
    pub const fn catalog_version(&self) -> CatalogVersion {
        self.catalog_version
    }

    /// Borrow the value only if its key and domain match.
    /// Use [`AuthenticatedCatalog::verify_admitted`] to also check its version.
    pub fn require_authority(
        &self,
        expected_key: &VerifyingKey,
        expected_domain: &str,
    ) -> Result<&T, CatalogError> {
        if self.verifying_key() != expected_key || self.domain() != expected_domain {
            return Err(CatalogError::AuthorityMismatch);
        }
        Ok(&self.entry)
    }
}

impl<T> AdmittedHandle<&T> {
    /// Borrow the authenticated entry.
    pub const fn get(&self) -> &T {
        self.entry
    }
}

fn domain_text(domain: &[u8]) -> Result<&str, CatalogError> {
    if domain.is_empty() {
        return Err(CatalogError::EmptyDomain);
    }
    str::from_utf8(domain).map_err(|_| CatalogError::InvalidDomainEncoding)
}

fn header_len(tag: &[u8], domain: &[u8], limit: usize) -> Result<usize, CatalogError> {
    let len = field_end(tag.len(), domain, limit)?;
    domain_text(domain)?;
    Ok(len)
}

fn start_payload(tag: &[u8], domain: &[u8], len: usize) -> Result<Vec<u8>, CatalogError> {
    let mut out = Vec::new();
    out.try_reserve_exact(len)
        .map_err(|_| CatalogError::PayloadTooLarge)?;
    out.extend_from_slice(tag);
    write_bytes(&mut out, domain, len)?;
    Ok(out)
}

fn encode_len(len: usize) -> Result<[u8; 4], CatalogError> {
    Ok(u32::try_from(len)
        .map_err(|_| CatalogError::EncodingTooLarge)?
        .to_le_bytes())
}

fn checked_len(len: usize, more: usize, limit: usize) -> Result<usize, CatalogError> {
    len.checked_add(more)
        .filter(|&total| total <= limit)
        .ok_or(CatalogError::PayloadTooLarge)
}

fn field_end(len: usize, bytes: &[u8], limit: usize) -> Result<usize, CatalogError> {
    let len = checked_len(len, size_of::<u32>(), limit)?;
    let end = checked_len(len, bytes.len(), limit)?;
    encode_len(bytes.len())?;
    Ok(end)
}

fn entry_end(len: usize, id: &[u8], payload: &[u8], limit: usize) -> Result<usize, CatalogError> {
    if id.is_empty() {
        return Err(CatalogError::EmptyEntryId);
    }
    field_end(field_end(len, id, limit)?, payload, limit)
}

fn write_bytes(out: &mut Vec<u8>, bytes: &[u8], limit: usize) -> Result<(), CatalogError> {
    // Keep the reservation bound even if a CatalogEntry violates its stability contract.
    let len = checked_len(out.len(), size_of::<u32>(), limit)?;
    checked_len(len, bytes.len(), limit)?;
    out.extend_from_slice(&encode_len(bytes.len())?);
    out.extend_from_slice(bytes);
    Ok(())
}

fn write_entry(
    out: &mut Vec<u8>,
    id: &[u8],
    payload: &[u8],
    limit: usize,
) -> Result<(), CatalogError> {
    write_bytes(out, id, limit)?;
    write_bytes(out, payload, limit)
}

fn verify_signature(
    payload: &[u8],
    signature: &[u8; 64],
    key: &[u8; 32],
) -> Result<VerifyingKey, CatalogError> {
    VerifyingKey::from_bytes(key)
        .and_then(|key| {
            key.verify_strict(payload, &Signature::from_bytes(signature))?;
            Ok(key)
        })
        .map_err(|_| CatalogError::InvalidSignature)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    const VERSION: CatalogVersion = CatalogVersion {
        version: 1,
        sequence: 2,
        epoch: 3,
    };
    const DOMAIN: &str = "example.registry";

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

    fn signed_catalog(
        key: &SigningKey,
        domain: &str,
        version: CatalogVersion,
    ) -> AuthenticatedCatalog<Entry> {
        let entries = vec![Entry(b"a", b"identical")];
        let payload =
            AuthenticatedCatalog::signing_payload(domain.as_bytes(), version, &entries).unwrap();
        AuthenticatedCatalog::verify(
            domain.as_bytes(),
            version,
            entries,
            &key.sign(&payload).to_bytes(),
            key.verifying_key().as_bytes(),
        )
        .unwrap()
    }

    #[test]
    fn handles_reject_another_key_or_domain_even_with_identical_entries() {
        let key = SigningKey::from_bytes(&[1; 32]);
        let other_key = SigningKey::from_bytes(&[2; 32]);
        let trusted = signed_catalog(&key, DOMAIN, VERSION);
        let expected_key = key.verifying_key();

        for (issuer, domain) in [(&other_key, DOMAIN), (&key, "example.other")] {
            let catalog = signed_catalog(issuer, domain, VERSION);
            let admitted = catalog.admit(b"a").unwrap();
            let entry = Entry(b"a", b"identical");
            let payload = QualificationHandle::signing_payload(domain.as_bytes(), &entry).unwrap();
            let qualified = QualificationHandle::verify(
                domain.as_bytes(),
                entry,
                &issuer.sign(&payload).to_bytes(),
                issuer.verifying_key().as_bytes(),
            )
            .unwrap();

            assert_eq!(qualified.verifying_key(), &issuer.verifying_key());
            assert_eq!(qualified.domain(), domain);
            assert_eq!(
                qualified.require_authority(&issuer.verifying_key(), domain),
                Ok(admitted.get())
            );
            assert_eq!(
                qualified.require_authority(&expected_key, DOMAIN),
                Err(CatalogError::AuthorityMismatch)
            );
            assert_eq!(admitted.verifying_key(), &issuer.verifying_key());
            assert_eq!(admitted.domain(), domain);
            assert_eq!(admitted.catalog_version(), VERSION);
            assert_eq!(
                admitted.require_authority(&expected_key, DOMAIN),
                Err(CatalogError::AuthorityMismatch)
            );
            assert_eq!(
                trusted.verify_admitted(&admitted),
                Err(CatalogError::AuthorityMismatch)
            );
            assert_eq!(catalog.verify_admitted(&admitted), Ok(&admitted.get()));
        }
    }

    #[test]
    fn admission_is_bound_to_all_snapshot_counters() {
        let key = SigningKey::from_bytes(&[1; 32]);
        let trusted = signed_catalog(&key, DOMAIN, VERSION);
        for version in [
            CatalogVersion {
                version: 2,
                ..VERSION
            },
            CatalogVersion {
                sequence: 3,
                ..VERSION
            },
            CatalogVersion {
                epoch: 4,
                ..VERSION
            },
        ] {
            let catalog = signed_catalog(&key, DOMAIN, version);
            let admitted = catalog.admit(b"a").unwrap();
            assert!(
                admitted
                    .require_authority(&key.verifying_key(), DOMAIN)
                    .is_ok()
            );
            assert_eq!(
                trusted.verify_admitted(&admitted),
                Err(CatalogError::AuthorityMismatch)
            );
        }
    }

    #[test]
    fn shared_entry_data_counts_toward_the_aggregate_limit() {
        let shared = &[1; 40];
        let entries = [Entry(b"a", shared), Entry(b"b", shared)];
        assert!(
            AuthenticatedCatalog::signing_payload_with_limit(b"d", VERSION, &entries[..1], 128)
                .is_ok()
        );
        assert_eq!(
            AuthenticatedCatalog::signing_payload_with_limit(b"d", VERSION, &entries, 128),
            Err(CatalogError::PayloadTooLarge),
        );
        let payload = AuthenticatedCatalog::signing_payload(b"d", VERSION, &entries).unwrap();
        assert_eq!(
            AuthenticatedCatalog::signing_payload_with_limit(
                b"d",
                VERSION,
                &entries,
                payload.len()
            ),
            Ok(payload),
        );
    }

    #[test]
    fn signing_rejects_domains_that_cannot_be_exposed_losslessly() {
        let entry = Entry(b"a", b"item");
        assert_eq!(
            QualificationHandle::signing_payload(b"\xff", &entry),
            Err(CatalogError::InvalidDomainEncoding),
        );
        assert_eq!(
            AuthenticatedCatalog::signing_payload(b"\xff", VERSION, &[entry]),
            Err(CatalogError::InvalidDomainEncoding),
        );
    }
}
