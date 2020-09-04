use algebra::bytes::ToBytes;
use zexe_cp::crh::FixedLengthCRH;

use crypto_primitives::sparse_merkle_tree::{
    MerkleIndex, MerkleTreeParameters, MerkleTreePath, SparseMerkleTree,
};
use single_step_avd::SingleStepAVD;

use crate::Error;

use std::{collections::HashMap, hash::Hash, io::{Write, Cursor}};
use rand::Rng;

pub mod constraints;

pub struct HistoryTree<P: MerkleTreeParameters, D: Hash + ToBytes + Eq + Clone> {
    tree: SparseMerkleTree<P>,
    digest_d: HashMap<D, MerkleIndex>,
    epoch: MerkleIndex,
}

impl<P: MerkleTreeParameters, D: Hash + ToBytes + Eq + Clone> HistoryTree<P, D> {
    pub fn new(hash_parameters: &<P::H as FixedLengthCRH>::Parameters) -> Result<Self, Error> {
        Ok(HistoryTree {
            tree: SparseMerkleTree::<P>::new(&<[u8; 32]>::default(), hash_parameters)?,
            digest_d: HashMap::new(),
            epoch: 0,
        })
    }

    // TODO: Manage digest lifetimes so as not to store clones
    pub fn append_digest(&mut self, digest: &D) -> Result<(), Error> {
        self.tree.update(self.epoch, &digest_to_bytes(digest)?)?;
        self.digest_d.insert(digest.clone(), self.epoch);
        self.epoch += 1;
        Ok(())
    }

    pub fn lookup_path(&self, epoch: MerkleIndex) -> Result<MerkleTreePath<P>, Error> {
        self.tree.lookup(epoch)
    }

    pub fn lookup_digest(&self, digest: &D) -> Option<MerkleIndex> {
        self.digest_d.get(digest).cloned()
    }
}

pub struct SingleStepUpdateProof<SSAVD: SingleStepAVD, HTParams: MerkleTreeParameters>
{
    pub ssavd_proof: SSAVD::UpdateProof,
    pub history_tree_proof: MerkleTreePath<HTParams>,
    pub prev_ssavd_digest: SSAVD::Digest,
    pub new_ssavd_digest: SSAVD::Digest,
    pub prev_digest: <<HTParams as MerkleTreeParameters>::H as FixedLengthCRH>::Output,
    pub new_digest: <<HTParams as MerkleTreeParameters>::H as FixedLengthCRH>::Output,
    pub prev_epoch: u64,
}

impl<SSAVD: SingleStepAVD, HTParams: MerkleTreeParameters> Default for SingleStepUpdateProof<SSAVD, HTParams>{
    fn default() -> Self {
        Self {
            ssavd_proof: SSAVD::UpdateProof::default(),
            history_tree_proof: <MerkleTreePath<HTParams>>::default(),
            prev_ssavd_digest: SSAVD::Digest::default(),
            new_ssavd_digest: SSAVD::Digest::default(),
            prev_digest: Default::default(),
            new_digest: Default::default(),
            prev_epoch: Default::default(),
        }
    }
}

pub struct SingleStepAVDWithHistory<SSAVD: SingleStepAVD, HTParams: MerkleTreeParameters>{
    ssavd: SSAVD,
    history_tree: HistoryTree<HTParams, <HTParams::H as FixedLengthCRH>::Output>,
    digest: <HTParams::H as FixedLengthCRH>::Output,
}

#[derive(Clone, Eq, PartialEq, Hash)]
pub struct Digest<HTParams: MerkleTreeParameters> {
    epoch: u64,
    digest: <HTParams::H as FixedLengthCRH>::Output,
}

pub struct LookupProof<SSAVD: SingleStepAVD, HTParams: MerkleTreeParameters> {
    ssavd_proof: SSAVD::LookupProof,
    ssavd_digest: SSAVD::Digest,
    history_tree_digest: <HTParams::H as FixedLengthCRH>::Output,
}

pub struct HistoryProof<SSAVD: SingleStepAVD, HTParams: MerkleTreeParameters> {
    history_tree_proof: MerkleTreePath<HTParams>,
    ssavd_digest: SSAVD::Digest,
    history_tree_digest: <HTParams::H as FixedLengthCRH>::Output,
}

impl<SSAVD: SingleStepAVD, HTParams: MerkleTreeParameters> SingleStepAVDWithHistory<SSAVD, HTParams> {

    pub fn setup<R: Rng>(rng: &mut R)
        -> Result<(SSAVD::PublicParameters, <HTParams::H as FixedLengthCRH>::Parameters), Error> {
        Ok((
            SSAVD::setup(rng)?,
            <HTParams::H as FixedLengthCRH>::setup(rng)?,
        ))
    }

    //TODO: Double storing SSAVD public parameters (also stored in MerkleTreeAVD)
    //TODO: Double storing hash parameters if shared across SSAVD and history tree
    pub fn new<R: Rng>(
        rng: &mut R,
        ssavd_pp: &SSAVD::PublicParameters,
        history_tree_parameters: &<HTParams::H as FixedLengthCRH>::Parameters,
    ) -> Result<Self, Error>{
        let ssavd = SSAVD::new(rng, ssavd_pp)?;
        let history_tree = HistoryTree::new(history_tree_parameters)?;
        let digest = hash_to_final_digest::<SSAVD, HTParams::H>(
            history_tree_parameters,
            &ssavd.digest()?,
            &history_tree.tree.root,
            &history_tree.epoch,
        )?;
        Ok(SingleStepAVDWithHistory{
            ssavd: ssavd,
            history_tree: history_tree,
            digest: digest,
        })
    }

    pub fn digest(&self) -> Digest<HTParams> {
        Digest {
            epoch: self.history_tree.epoch.clone(),
            digest: self.digest.clone(),
        }
    }

    pub fn update(
        &mut self,
        key: &[u8; 32],
        value: &[u8; 32],
    ) -> Result<SingleStepUpdateProof<SSAVD, HTParams>, Error>{
        let prev_ssavd_digest = self.ssavd.digest()?;
        let (new_ssavd_digest, ssavd_proof) = self.ssavd.update(key, value)?;
        let prev_epoch = self.history_tree.epoch.clone();
        let prev_digest = self.digest.clone();
        self.history_tree.append_digest(&prev_digest)?;
        let history_tree_proof = self.history_tree.lookup_path(prev_epoch)?;

        // Update digest
        self.digest = hash_to_final_digest::<SSAVD, HTParams::H>(
            &self.history_tree.tree.hash_parameters,
            &new_ssavd_digest,
            &self.history_tree.tree.root,
            &self.history_tree.epoch,
        )?;

        Ok(SingleStepUpdateProof{
            ssavd_proof: ssavd_proof,
            history_tree_proof: history_tree_proof,
            prev_ssavd_digest: prev_ssavd_digest,
            new_ssavd_digest: new_ssavd_digest,
            prev_digest: prev_digest,
            new_digest: self.digest.clone(),
            prev_epoch: prev_epoch,
        })
    }

    pub fn batch_update(
        &mut self,
        kvs: &Vec<([u8; 32], [u8; 32])>,
    ) -> Result<SingleStepUpdateProof<SSAVD, HTParams>, Error>{
        let prev_ssavd_digest = self.ssavd.digest()?;
        let (new_ssavd_digest, ssavd_proof) = self.ssavd.batch_update(kvs)?;
        let prev_epoch = self.history_tree.epoch.clone();
        let prev_digest = self.digest.clone();
        self.history_tree.append_digest(&prev_digest)?;
        let history_tree_proof = self.history_tree.lookup_path(prev_epoch)?;

        // Update digest
        self.digest = hash_to_final_digest::<SSAVD, HTParams::H>(
            &self.history_tree.tree.hash_parameters,
            &new_ssavd_digest,
            &self.history_tree.tree.root,
            &self.history_tree.epoch,
        )?;

        Ok(SingleStepUpdateProof{
            ssavd_proof: ssavd_proof,
            history_tree_proof: history_tree_proof,
            prev_ssavd_digest: prev_ssavd_digest,
            new_ssavd_digest: new_ssavd_digest,
            prev_digest: prev_digest,
            new_digest: self.digest.clone(),
            prev_epoch: prev_epoch,
        })
    }

    fn lookup(
        &self,
        key: &[u8; 32],
    ) -> Result<(Option<(u64, [u8; 32])>, LookupProof<SSAVD, HTParams>), Error>{
        let (result, ssavd_digest, ssavd_proof) = self.ssavd.lookup(key)?;
        Ok((
            result,
            LookupProof {
                ssavd_proof: ssavd_proof,
                ssavd_digest: ssavd_digest,
                history_tree_digest: self.history_tree.tree.root.clone(),
            }
        ))
    }

    fn verify_lookup(
        ssavd_pp: &SSAVD::PublicParameters,
        history_tree_pp: &<HTParams::H as FixedLengthCRH>::Parameters,
        key: &[u8; 32],
        value: &Option<(u64, [u8; 32])>,
        digest: &Digest<HTParams>,
        proof: &LookupProof<SSAVD, HTParams>,
    ) -> Result<bool, Error>{
        Ok(
            SSAVD::verify_lookup(ssavd_pp, key, value, &proof.ssavd_digest, &proof.ssavd_proof)? &&
                digest.digest ==
                    hash_to_final_digest::<SSAVD, HTParams::H>(
                        history_tree_pp,
                        &proof.ssavd_digest,
                        &proof.history_tree_digest,
                        &digest.epoch,
                    )?
        )
    }

    fn lookup_history(
        &self,
        prev_digest: &Digest<HTParams>,
    ) -> Result<Option<HistoryProof<SSAVD, HTParams>>, Error> {
        match (
            self.history_tree.lookup_digest(&prev_digest.digest),
            self.history_tree.lookup_path(prev_digest.epoch)?,
        ) {
            (Some(epoch), path) if epoch == prev_digest.epoch => {
                Ok(Some(HistoryProof {
                    history_tree_proof: path,
                    ssavd_digest: self.ssavd.digest()?,
                    history_tree_digest: self.history_tree.tree.root.clone(),
                }))
            },
            _ => Ok(None),
        }
    }

    fn verify_history(
        history_tree_pp: &<HTParams::H as FixedLengthCRH>::Parameters,
        prev_digest: &Digest<HTParams>,
        current_digest: &Digest<HTParams>,
        proof: &HistoryProof<SSAVD, HTParams>,
    ) -> Result<bool, Error> {
        Ok(
            proof.history_tree_proof.verify(
                &proof.history_tree_digest,
                &digest_to_bytes(&prev_digest.digest)?,
                prev_digest.epoch,
                history_tree_pp,
            )? &&
                current_digest.digest ==
                    hash_to_final_digest::<SSAVD, HTParams::H>(
                        history_tree_pp,
                        &proof.ssavd_digest,
                        &proof.history_tree_digest,
                        &current_digest.epoch,
                    )?
        )
    }
}


//TODO: Optimization: Pick a hash function compatible with digest sizes and epoch size -- current fix for PedersenHash is hash twice
pub fn hash_to_final_digest<SSAVD: SingleStepAVD, H: FixedLengthCRH>(
    parameters: &H::Parameters,
    ssavd_digest: &SSAVD::Digest,
    history_tree_digest: &H::Output,
    epoch: &u64,
) -> Result<H::Output, Error> {
    // Hash together digests
    let mut buffer1 = [0u8; 128];
    let mut writer1 = Cursor::new(&mut buffer1[..]);
    ssavd_digest.write(&mut writer1)?;
    history_tree_digest.write(&mut writer1)?;
    let digests_hash = H::evaluate(&parameters, &buffer1[..(H::INPUT_SIZE_BITS / 8)])?;

    // Hash in epoch
    let mut buffer2 = [0u8; 128];
    let mut writer2 = Cursor::new(&mut buffer2[..]);
    writer2.write(&epoch.to_le_bytes())?;
    digests_hash.write(&mut writer2)?;
    H::evaluate(&parameters, &buffer2[..(H::INPUT_SIZE_BITS / 8)])
}

pub fn digest_to_bytes<D: ToBytes>(digest: &D) -> Result<[u8; 128], Error> {
    let mut buffer = [0_u8; 128];
    let mut writer = Cursor::new(&mut buffer[..]);
    digest.write(&mut writer)?;
    Ok(buffer)
}

#[cfg(test)]
mod test {
    use super::*;
    use algebra::ed_on_bls12_381::{EdwardsAffine as JubJub, Fq};
    use rand::{rngs::StdRng, SeedableRng};
    use zexe_cp::crh::{
        pedersen::{PedersenCRH, PedersenWindow},
    };

    use single_step_avd::{
        merkle_tree_avd::{
            MerkleTreeAVDParameters,
            MerkleTreeAVD,
        },
    };
    use crypto_primitives::sparse_merkle_tree::MerkleDepth;

    #[derive(Clone)]
    pub struct Window4x256;

    impl PedersenWindow for Window4x256 {
        const WINDOW_SIZE: usize = 4;
        const NUM_WINDOWS: usize = 256;
    }

    type H = PedersenCRH<JubJub, Window4x256>;

    #[derive(Clone)]
    pub struct MerkleTreeTestParameters;

    impl MerkleTreeParameters for MerkleTreeTestParameters {
        const DEPTH: MerkleDepth = 4;
        type H = H;
    }

    #[derive(Clone)]
    pub struct MerkleTreeAVDTestParameters;

    impl MerkleTreeAVDParameters for MerkleTreeAVDTestParameters {
        const MAX_UPDATE_BATCH_SIZE: u64 = 4;
        const MAX_OPEN_ADDRESSING_PROBES: u8 = 2;
        type MerkleTreeParameters = MerkleTreeTestParameters;
    }

    type TestMerkleTreeAVD = MerkleTreeAVD<MerkleTreeAVDTestParameters>;
    type TestAVDWithHistory = SingleStepAVDWithHistory<TestMerkleTreeAVD, MerkleTreeTestParameters>;

    #[test]
    fn lookup_test() {
        let mut rng = StdRng::seed_from_u64(0_u64);
        let (ssavd_pp, crh_pp) = TestAVDWithHistory::setup(&mut rng).unwrap();
        let mut avd = TestAVDWithHistory::new(&mut rng, &ssavd_pp, &crh_pp).unwrap();
        avd.update(&[1_u8; 32], &[2_u8; 32]).unwrap();
        let digest = avd.digest();

        let (value, lookup_proof) = avd.lookup(&[1_u8; 32]).unwrap();
        let result = TestAVDWithHistory::verify_lookup(
            &ssavd_pp,
            &crh_pp,
            &[1_u8; 32],
            &value,
            &digest,
            &lookup_proof,
        ).unwrap();
        assert!(result);
    }

    #[test]
    fn history_test() {
        let mut rng = StdRng::seed_from_u64(0_u64);
        let (ssavd_pp, crh_pp) = TestAVDWithHistory::setup(&mut rng).unwrap();
        let mut avd = TestAVDWithHistory::new(&mut rng, &ssavd_pp, &crh_pp).unwrap();
        avd.update(&[1_u8; 32], &[2_u8; 32]).unwrap();
        let prev_digest = avd.digest();
        assert_eq!(prev_digest.epoch, 1);
        avd.update(&[1_u8; 32], &[3_u8; 32]).unwrap();
        let curr_digest = avd.digest();
        assert_eq!(curr_digest.epoch, 2);

        let history_proof = avd.lookup_history(&prev_digest).unwrap().unwrap();
        let result = TestAVDWithHistory::verify_history(
            &crh_pp,
            &prev_digest,
            &curr_digest,
            &history_proof,
        ).unwrap();
        assert!(result);

        let invalid_history_proof = avd.lookup_history(
            &Digest{epoch: 1, digest: Default::default()}
        ).unwrap();
        assert!(invalid_history_proof.is_none());
    }

}
