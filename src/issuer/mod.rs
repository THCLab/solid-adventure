use std::path::{Path, PathBuf};

use crate::{error::Error, kerl::KERL, tel::Tel};
use keri::{
    database::sled::SledEventDatabase,
    derivation::{self_addressing::SelfAddressing, self_signing::SelfSigning},
    event::sections::seal::{EventSeal, Seal},
    prefix::{BasicPrefix, IdentifierPrefix, Prefix, SelfAddressingPrefix},
    signer::KeyManager,
};
use teliox::{
    database::EventDatabase,
    event::{manager_event::Config, verifiable_event::VerifiableEvent, Event},
    seal::EventSourceSeal,
    state::vc_state::TelState,
};

pub struct Controller {
    kerl: KERL,
    tel: Tel,
}

impl Controller {
    pub fn new(root: &Path, tel_db: &Path) -> Self {
        let db = SledEventDatabase::new(root).unwrap();
        let tel_db = EventDatabase::new(tel_db).unwrap();
        let tel = Tel::new(
            tel_db,
            keri::event::SerializationFormats::JSON,
            SelfAddressing::Blake3_256,
        );

        Controller {
            kerl: KERL::new(db, IdentifierPrefix::default()).unwrap(),
            tel,
        }
    }

    pub fn init<K: KeyManager>(
        kel_db_path: &Path,
        tel_db_path: &Path,
        km: &K,
        backers: Option<Vec<IdentifierPrefix>>,
        backer_threshold: u64,
    ) -> Result<Self, Error> {
        let mut controller = Controller::new(kel_db_path, tel_db_path);
        controller.incept_kel(km)?;
        controller.incept_tel(km, backers, backer_threshold)?;
        Ok(controller)
    }

    /// Generate and process tel inception event for given backers and backer
    /// threshold. None in backers argument sets config to no backers.
    fn incept_tel<K: KeyManager>(
        &mut self,
        km: &K,
        backers: Option<Vec<IdentifierPrefix>>,
        backer_threshold: u64,
    ) -> Result<(), Error> {
        let (config, b) = match backers {
            Some(backers) => (vec![], backers),
            None => (vec![Config::NoBackers], vec![]),
        };
        let vcp = self.tel.make_inception_event(
            self.kerl.get_state().unwrap().unwrap().prefix.clone(),
            config,
            backer_threshold,
            b,
        )?;

        // create vcp seal which will be inserted into issuer kel (ixn event)
        let vcp_seal = Seal::Event(EventSeal {
            prefix: vcp.clone().prefix,
            sn: vcp.sn,
            event_digest: SelfAddressing::Blake3_256.derive(&vcp.serialize()?),
        });

        let ixn = self.kerl.make_ixn_with_seal(&vec![vcp_seal], km)?;

        let ixn_source_seal = EventSourceSeal {
            sn: ixn.event_message.event.sn,
            digest: SelfAddressing::Blake3_256.derive(&ixn.event_message.serialize()?),
        };

        // before applying vcp to management tel, insert anchor event seal to be able to verify that operation.
        let verifiable_vcp =
            VerifiableEvent::new(Event::Management(vcp.clone()), ixn_source_seal.into());
        self.tel.process(verifiable_vcp)?;

        Ok(())
    }

    // Generate and process kel inception event.
    fn incept_kel<K: KeyManager>(&mut self, km: &K) -> Result<(), Error> {
        self.kerl.incept(km)?;
        Ok(())
    }

    // Generate and process management tel rotation event for given backers to
    // add (ba) and backers to remove (br).
    pub fn update_backers<K: KeyManager>(
        &mut self,
        ba: &[IdentifierPrefix],
        br: &[IdentifierPrefix],
        km: &K,
    ) -> Result<(), Error> {
        let rcp = self.tel.make_rotation_event(ba, br)?;

        // create rcp seal which will be inserted into issuer kel (ixn event)
        let rcp_seal = Seal::Event(EventSeal {
            prefix: rcp.prefix.clone(),
            sn: rcp.sn,
            event_digest: SelfAddressing::Blake3_256.derive(&rcp.serialize()?),
        });

        let ixn = self.kerl.make_ixn_with_seal(&vec![rcp_seal], km)?;

        let ixn_source_seal = EventSourceSeal {
            sn: ixn.event_message.event.sn,
            digest: SelfAddressing::Blake3_256.derive(&ixn.serialize()?),
        };

        // before applying vcp to management tel, insert anchor event seal to be able to verify that operation.
        let verifiable_rcp =
            VerifiableEvent::new(Event::Management(rcp.clone()), ixn_source_seal.into());
        self.tel.process(verifiable_rcp.clone())?;
        Ok(())
    }

    pub fn issue<K: KeyManager>(&mut self, message: &str, km: &K) -> Result<Vec<u8>, Error> {
        let iss = self.tel.make_issuance_event(message)?;
        // create vcp seal which will be inserted into issuer kel (ixn event)
        let iss_seal = Seal::Event(EventSeal {
            prefix: iss.prefix.clone(),
            sn: iss.sn,
            event_digest: SelfAddressing::Blake3_256.derive(&iss.serialize()?),
        });

        let ixn = self.kerl.make_ixn_with_seal(&vec![iss_seal], km)?;

        let ixn_source_seal = EventSourceSeal {
            sn: ixn.event_message.event.sn,
            digest: SelfAddressing::Blake3_256.derive(&ixn.event_message.serialize()?),
        };

        let verifiable_vcp = VerifiableEvent::new(Event::Vc(iss.clone()), ixn_source_seal.into());
        self.tel.process(verifiable_vcp.clone())?;
        km.sign(&message.as_bytes().to_vec()).map_err(|e| e.into())
    }

    pub fn revoke<K: KeyManager>(&mut self, message: &str, km: &K) -> Result<(), Error> {
        let message_id = SelfAddressing::Blake3_256.derive(message.as_bytes());
        let rev_event = self.tel.make_revoke_event(&message_id)?;
        // create rev seal which will be inserted into issuer kel (ixn event)
        let rev_seal = Seal::Event(EventSeal {
            prefix: rev_event.prefix.clone(),
            sn: rev_event.sn,
            event_digest: SelfAddressing::Blake3_256.derive(&rev_event.serialize()?),
        });

        let ixn = self.kerl.make_ixn_with_seal(&vec![rev_seal], km)?;

        // Make source seal.
        let ixn_source_seal = EventSourceSeal {
            sn: ixn.event_message.event.sn,
            digest: SelfAddressing::Blake3_256.derive(&ixn.serialize()?),
        };

        let verifiable_rev =
            VerifiableEvent::new(Event::Vc(rev_event.clone()), ixn_source_seal.into());

        self.tel.process(verifiable_rev.clone())?;
        Ok(())
    }

    pub fn rotate<K: KeyManager>(&self, km: &K) -> Result<(), Error> {
        self.kerl.rotate(km)?;
        Ok(())
    }

    /// Check the state of message of given digest.
    pub fn get_vc_state(&self, hash: &SelfAddressingPrefix) -> Result<TelState, Error> {
        self.tel.get_vc_state(hash).map_err(|e| e.into())
    }

    pub fn get_tel(&self, hash: &SelfAddressingPrefix) -> Result<Vec<VerifiableEvent>, Error> {
        self.tel.get_tel(hash)
    }

    /// Returns keys that was used to sign message of given hash. Returns error,
    /// if message was revoked or not yet issued.
    pub fn get_pub_key(
        &self,
        message_hash: SelfAddressingPrefix,
    ) -> Result<Vec<BasicPrefix>, Error> {
        // Get last event vc event and its source seal.
        let source_seal: EventSourceSeal = self
            .tel
            .get_tel(&message_hash)?
            // TODO what if events are out of order?
            .last()
            .ok_or(Error::Generic("No events in tel".into()))?
            .seal
            .seal
            .clone();

        let k = self.kerl.get_state_for_seal(
            &self.tel.get_issuer()?,
            source_seal.sn,
            &source_seal.digest,
        )?;
        match k {
            Some(state) => Ok(state.current.public_keys),
            None => Err(Error::Generic("No key data".into())),
        }
    }

    /// Verify signature for given message.
    pub fn verify(&self, message: &str, signature: &[u8]) -> Result<bool, Error> {
        let message_hash = SelfAddressing::Blake3_256.derive(message.as_bytes());
        match self.get_vc_state(&message_hash)? {
            TelState::NotIsuued => Err(Error::Generic("Not yet issued".into())),
            TelState::Issued(_) => {
                let key = self.get_pub_key(message_hash)?;
                Ok(key.into_iter().fold(true, |acc, k| {
                    let sspref = SelfSigning::Ed25519Sha512.derive(signature.to_vec());
                    acc && k.verify(message.as_bytes(), &sspref).unwrap()
                }))
            }
            TelState::Revoked => Err(Error::Generic("VC was revoked".into())),
        }
    }
}

#[cfg(test)]
mod test {
    use keri::{
        derivation::self_addressing::SelfAddressing,
        signer::{CryptoBox, KeyManager},
    };
    use teliox::state::vc_state::TelState;

    use crate::{error::Error, issuer::Controller};

    #[test]
    pub fn test_issuing() -> Result<(), Error> {
        use tempfile::Builder;
        // Create test db and key manager.
        let root = Builder::new().prefix("test-db").tempdir().unwrap();
        let tel_root = Builder::new().prefix("tel-test-db").tempdir().unwrap();
        let mut km = CryptoBox::new()?;

        let message = "some vc";

        let mut issuer = Controller::init(root.path(), tel_root.path(), &km, Some(vec![]), 0)?;

        // Chcek if tel inception event is in db.
        let o = issuer.tel.get_management_events()?;
        assert!(o.is_some());

        let message_hash = SelfAddressing::Blake3_256.derive(message.as_bytes());

        let signature = issuer.issue(message, &km)?;
        let verification_result = issuer.verify(message, &signature);
        assert!(matches!(verification_result, Ok(true)));

        // Chcek if iss event is in db.
        let o = issuer.get_tel(&message_hash)?;
        assert_eq!(o.len(), 1);

        let state = issuer.get_vc_state(&message_hash)?;
        assert!(matches!(state, TelState::Issued(_)));

        // Try to verify message after key rotation.
        km.rotate()?;
        issuer.rotate(&km)?;

        let verification_result = issuer.verify(message, &signature);
        assert!(matches!(verification_result, Ok(true)));

        issuer.revoke(message, &km)?;
        let state = issuer.get_vc_state(&message_hash)?;
        assert!(matches!(state, TelState::Revoked));

        // Check if revoke event is in db.
        let o = issuer.get_tel(&message_hash)?;
        assert_eq!(o.len(), 2);

        // Message verification should return error, because it was revoked.
        let verification_result = issuer.verify(message, &signature);
        assert!(verification_result.is_err());

        Ok(())
    }
}
