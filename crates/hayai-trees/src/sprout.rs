//! The Sprout note commitment frontier.
//!
//! The Sprout tree has depth 29. MerkleCRH^Sprout is the SHA-256 compression function on
//! the 64 bytes `left || right`, with the SHA-256 initial state and without padding
//! (protocol specification §5.4.1.3). The empty leaf is 32 zero bytes. The frontier is the
//! upstream `incrementalmerkletree::Frontier<_, 29>`, as in Zakura
//! (`zakura-chain/src/sprout/tree.rs`).
//!
//! A JoinSplit adds two commitments, so the frontier appends leaf by leaf and has no
//! batched path.

use std::io::{self, Read, Write};
use std::sync::LazyLock;

use hayai_crypto::incrementalmerkletree::frontier::Frontier;
use hayai_crypto::incrementalmerkletree::{Hashable, Level};
use hayai_crypto::zcash_encoding::Optional;
use hayai_crypto::zcash_primitives::merkle_tree::{
    read_nonempty_frontier_v1, write_nonempty_frontier_v1, HashSer,
};

use crate::TreeError;

/// Depth of the Sprout note commitment tree.
pub const SPROUT_DEPTH: u8 = 29;

/// The initial state of SHA-256.
const SHA256_IV: [u32; 8] = [
    0x6a09_e667,
    0xbb67_ae85,
    0x3c6e_f372,
    0xa54f_f53a,
    0x510e_527f,
    0x9b05_688c,
    0x1f83_d9ab,
    0x5be0_cd19,
];

/// MerkleCRH^Sprout.
fn merkle_crh_sprout(left: &[u8; 32], right: &[u8; 32]) -> [u8; 32] {
    let mut block = [0u8; 64];
    block[..32].copy_from_slice(left);
    block[32..].copy_from_slice(right);
    let mut state = SHA256_IV;
    sha2::compress256(&mut state, &[block.into()]);
    let mut out = [0u8; 32];
    for (bytes, word) in out.as_chunks_mut::<4>().0.iter_mut().zip(state) {
        *bytes = word.to_be_bytes();
    }
    out
}

/// The root of the empty subtree of each level: entry 0 is the empty leaf.
static EMPTY_ROOTS: LazyLock<[[u8; 32]; SPROUT_DEPTH as usize + 1]> = LazyLock::new(|| {
    let mut roots = [[0u8; 32]; SPROUT_DEPTH as usize + 1];
    for level in 1..roots.len() {
        roots[level] = merkle_crh_sprout(&roots[level - 1], &roots[level - 1]);
    }
    roots
});

/// A node of the Sprout note commitment tree. A leaf is a note commitment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SproutNode(pub [u8; 32]);

impl Hashable for SproutNode {
    fn empty_leaf() -> Self {
        Self([0u8; 32])
    }

    fn combine(_level: Level, left: &Self, right: &Self) -> Self {
        Self(merkle_crh_sprout(&left.0, &right.0))
    }

    fn empty_root(level: Level) -> Self {
        Self(EMPTY_ROOTS[usize::from(u8::from(level))])
    }
}

impl HashSer for SproutNode {
    fn read<R: Read>(mut reader: R) -> io::Result<Self> {
        let mut bytes = [0u8; 32];
        reader.read_exact(&mut bytes)?;
        Ok(Self(bytes))
    }

    fn write<W: Write>(&self, mut writer: W) -> io::Result<()> {
        writer.write_all(&self.0)
    }
}

/// The Sprout note commitment frontier, with its root.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SproutFrontier {
    inner: Frontier<SproutNode, SPROUT_DEPTH>,
    root: [u8; 32],
}

impl SproutFrontier {
    /// The empty tree.
    pub fn empty() -> Self {
        Self::from_frontier(Frontier::empty())
    }

    /// Wraps an upstream frontier.
    pub fn from_frontier(inner: Frontier<SproutNode, SPROUT_DEPTH>) -> Self {
        let root = inner.root().0;
        Self { inner, root }
    }

    /// The upstream frontier.
    pub fn frontier(&self) -> &Frontier<SproutNode, SPROUT_DEPTH> {
        &self.inner
    }

    /// Appends `commitments` in order and returns the new root.
    pub fn append_many(&mut self, commitments: &[[u8; 32]]) -> Result<[u8; 32], TreeError> {
        // Spec §3.8: a block must not add commitments past the capacity of 2^29 leaves.
        let capacity = 1u64 << SPROUT_DEPTH;
        let requested = self.inner.tree_size() + commitments.len() as u64;
        if requested > capacity {
            return Err(TreeError::Full {
                capacity,
                requested,
            });
        }
        for commitment in commitments {
            assert!(
                self.inner.append(SproutNode(*commitment)),
                "the capacity check found room for the leaf"
            );
        }
        self.root = self.inner.root().0;
        Ok(self.root)
    }

    /// The root of the current tree.
    pub fn root(&self) -> [u8; 32] {
        self.root
    }

    /// Writes the frontier in the encoding of upstream `write_frontier_v1`. The upstream
    /// function takes a tree of depth 32 only (`zcash_primitives` 0.30.1,
    /// `merkle_tree.rs`), so this function calls its two parts.
    pub fn write<W: Write>(&self, writer: W) -> io::Result<()> {
        Optional::write(writer, self.inner.value(), write_nonempty_frontier_v1)
    }

    /// Reads a frontier that [`SproutFrontier::write`] wrote.
    pub fn read<R: Read>(reader: R) -> io::Result<Self> {
        let invalid = || io::Error::new(io::ErrorKind::InvalidData, "invalid Sprout frontier");
        match Optional::read(reader, read_nonempty_frontier_v1)? {
            None => Ok(Self::empty()),
            Some(frontier) => Frontier::try_from(frontier)
                .map(Self::from_frontier)
                .map_err(|_| invalid()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex32(s: &str) -> [u8; 32] {
        let mut out = [0u8; 32];
        for (i, byte) in out.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&s[2 * i..2 * i + 2], 16).expect("hex digits");
        }
        out
    }

    // zcashd `IncrementalMerkleTree.cpp` (empty roots, leaf first) and
    // `src/test/data/merkle_commitments.json`, with the roots of depth 29, as
    // `zebra-chain` 13.0.1 publishes them (`src/sprout/tests/test_vectors.rs`).
    const EMPTY_ROOT_VECTORS: [&str; 30] = [
        "0000000000000000000000000000000000000000000000000000000000000000",
        "da5698be17b9b46962335799779fbeca8ce5d491c0d26243bafef9ea1837a9d8",
        "dc766fab492ccf3d1e49d4f374b5235fa56506aac2224d39f943fcd49202974c",
        "3f0a406181105968fdaee30679e3273c66b72bf9a7f5debbf3b5a0a26e359f92",
        "26b0052694fc42fdff93e6fb5a71d38c3dd7dc5b6ad710eb048c660233137fab",
        "0109ecc0722659ff83450b8f7b8846e67b2859f33c30d9b7acd5bf39cae54e31",
        "3f909b8ce3d7ffd8a5b30908f605a03b0db85169558ddc1da7bbbcc9b09fd325",
        "40460fa6bc692a06f47521a6725a547c028a6a240d8409f165e63cb54da2d23f",
        "8c085674249b43da1b9a31a0e820e81e75f342807b03b6b9e64983217bc2b38e",
        "a083450c1ba2a3a7be76fad9d13bc37be4bf83bd3e59fc375a36ba62dc620298",
        "1ddddabc2caa2de9eff9e18c8c5a39406d7936e889bc16cfabb144f5c0022682",
        "c22d8f0b5e4056e5f318ba22091cc07db5694fbeb5e87ef0d7e2c57ca352359e",
        "89a434ae1febd7687eceea21d07f20a2512449d08ce2eee55871cdb9d46c1233",
        "7333dbffbd11f09247a2b33a013ec4c4342029d851e22ba485d4461851370c15",
        "5dad844ab9466b70f745137195ca221b48f346abd145fb5efc23a8b4ba508022",
        "507e0dae81cbfbe457fd370ef1ca4201c2b6401083ddab440e4a038dc1e358c4",
        "bdcdb3293188c9807d808267018684cfece07ac35a42c00f2c79b4003825305d",
        "bab5800972a16c2c22530c66066d0a5867e987bed21a6d5a450b683cf1cfd709",
        "11aa0b4ad29b13b057a31619d6500d636cd735cdd07d811ea265ec4bcbbbd058",
        "5145b1b055c2df02b95675e3797b91de1b846d25003c0a803d08900728f2cd6a",
        "0323f2850bf3444f4b4c5c09a6057ec7169190f45acb9e46984ab3dfcec4f06a",
        "671546e26b1da1af754531e26d8a6a51073a57ddd72dc472efb43fcb257cffff",
        "bb23a9bba56de57cb284b0d2b01c642cf79c9a5563f0067a21292412145bd78a",
        "f30cc836b9f71b4e7ee3c72b1fd253268af9a27e9d7291a23d02821b21ddfd16",
        "58a2753dade103cecbcda50b5ebfce31e12d41d5841dcc95620f7b3d50a1b9a1",
        "925e6d474a5d8d3004f29da0dd78d30ae3824ce79dfe4934bb29ec3afaf3d521",
        "08f279618616bcdd4eadc9c7a9062691a59b43b07e2c1e237f17bd189cd6a8fe",
        "c92b32db42f42e2bf0a59df9055be5c669d3242df45357659b75ae2c27a76f50",
        "c0db2a74998c50eb7ba6534f6d410efc27c4bb88acb0222c7906ea28a327b511",
        "d7c612c817793191a1e68652121876d6b3bde40f4fa52bc314145ce6e5cdd259",
    ];
    const COMMITMENTS: [&str; 16] = [
        "62fdad9bfbf17c38ea626a9c9b8af8a748e6b4367c8494caf0ca592999e8b6ba",
        "68eb35bc5e1ddb80a761718e63a1ecf4d4977ae22cc19fa732b85515b2a4c943",
        "836045484077cf6390184ea7cd48b460e2d0f22b2293b69633bb152314a692fb",
        "92498a8295ea36d593eaee7cb8b55be3a3e37b8185d3807693184054cd574ae4",
        "ff7c360374a6508ae0904c782127ff5dce90918f3ee81cf92ef1b69afb8bf443",
        "68c4d0f69d1f18b756c2ee875c14f1c6cd38682e715ded14bf7e3c1c5610e9fc",
        "8b16cd3ec44875e4856e30344c0b4a68a6f929a68be5117b225b80926301e7b1",
        "50c0b43061c39191c3ec529734328b7f9cafeb6fd162cc49a4495442d9499a2d",
        "70ffdd5fa0f3aea18bd4700f1ac2e2e03cf5d4b7b857e8dd93b862a8319b9653",
        "d81ef64a0063573d80cd32222d8d04debbe807345ad7af2e9edf0f44bdfaf817",
        "8b92a4ec694271fe1b16cc0ea8a433bf19e78eb5ca733cc137f38e5ecb05789b",
        "04e963ab731e4aaaaaf931c3c039ea8c9d7904163936e19a8929434da9adeba3",
        "be3f6c181f162824191ecf1f78cae3ffb0ddfda671bb93277ce6ebc9201a0912",
        "1880967fc8226380a849c63532bba67990f7d0a10e9c90b848f58d634957c6e9",
        "c465bb2893cba233351094f259396301c23d73a6cf6f92bc63428a43f0dd8f8e",
        "84c834e7cb38d6f08d82f5cf4839b8920185174b11c7af771fd38dd02b206a20",
    ];
    const ROOTS: [&str; 16] = [
        "b8e10b6c157be92c43a733e2c9bddb963a2fb9ea80ebcb307acdcc5fc89f1656",
        "83a7754b8240699dd1b63bf70cf70db28ffeb74ef87ce2f4dd32c28ae5009f4f",
        "c45297124f50dcd3f78eed017afd1e30764cd74cdf0a57751978270fd0721359",
        "b61f588fcba9cea79e94376adae1c49583f716d2f20367141f1369a235b95c98",
        "a3165c1708f0cc028014b9bf925a81c30091091ca587624de853260cd151b524",
        "6bb8c538c550abdd26baa2a7510a4ae50a03dc00e52818b9db3e4ffaa29c1f41",
        "e04e4731085ba95e3fa7c8f3d5eb9a56af63363403b783bc68802629c3fe505b",
        "c3714ab74d8e3984e8b58a2b4806934d20f6e67d7246cf8f5b2762305294a0ea",
        "63657edeead4bc45610b6d5eb80714a0622aad5788119b7d9961453e3aacda21",
        "e31b80819221718440c5351525dbb902d60ed16b74865a2528510959a1960077",
        "872f13df2e12f5503c39100602930b0f91ea360e5905a9f5ceb45d459efc36b2",
        "bdd7105febb3590832e946aa590d07377d1366cf5e7267507efa399dd0febdbc",
        "0f45f4adcb846a8bb56833ca0cae96f2fb8747958daa191a46d0f9d93268260a",
        "41c6e456e2192ab74f72cb27c444a2734ca8ade5a4788c1bc2546118dda01778",
        "8261355fd9bafc52a08d738fed29a859fbe15f2e74a5353954b150be200d0e16",
        "90665cb8a43001f0655169952399590cd17f99165587c1dd842eb674fb9f0afe",
    ];

    #[test]
    fn the_empty_roots_are_the_published_roots() {
        for (level, expected) in EMPTY_ROOT_VECTORS.iter().enumerate() {
            let level = Level::from(u8::try_from(level).expect("30 levels"));
            assert_eq!(SproutNode::empty_root(level).0, hex32(expected));
        }
        assert_eq!(
            SproutFrontier::empty().root(),
            hex32(EMPTY_ROOT_VECTORS[29])
        );
    }

    #[test]
    fn each_append_gives_the_published_root() {
        let mut one_by_one = SproutFrontier::empty();
        for (i, (commitment, root)) in COMMITMENTS.iter().zip(ROOTS).enumerate() {
            let root = hex32(root);
            assert_eq!(one_by_one.append_many(&[hex32(commitment)]).unwrap(), root);
            assert_eq!(one_by_one.root(), root);
            assert_eq!(one_by_one.frontier().tree_size(), i as u64 + 1);
            // The same leaves in one call.
            let leaves: Vec<[u8; 32]> = COMMITMENTS[..=i].iter().map(|c| hex32(c)).collect();
            let mut at_once = SproutFrontier::empty();
            assert_eq!(at_once.append_many(&leaves).unwrap(), root);
            assert_eq!(at_once, one_by_one);
        }
    }

    #[test]
    fn the_frontier_encoding_round_trips() {
        let mut tree = SproutFrontier::empty();
        for commitment in COMMITMENTS {
            let mut bytes = Vec::new();
            tree.write(&mut bytes).unwrap();
            assert_eq!(SproutFrontier::read(&bytes[..]).unwrap(), tree);
            tree.append_many(&[hex32(commitment)]).unwrap();
        }
        // A position that the depth of the tree does not hold.
        let mut bytes = vec![1u8];
        bytes.extend_from_slice(&(1u64 << SPROUT_DEPTH).to_le_bytes());
        bytes.extend_from_slice(&[0u8; 32]);
        bytes.extend_from_slice(&[0, 0]);
        let Err(_) = SproutFrontier::read(&bytes[..]) else {
            panic!("a position outside the tree is rejected");
        };
    }

    #[test]
    fn a_full_tree_takes_no_leaf() {
        use hayai_crypto::incrementalmerkletree::Position;
        let last = (1u64 << SPROUT_DEPTH) - 1;
        let ommers = vec![SproutNode([1; 32]); SPROUT_DEPTH as usize];
        let full = Frontier::from_parts(Position::from(last), SproutNode([2; 32]), ommers)
            .expect("consistent parts");
        let mut tree = SproutFrontier::from_frontier(full);
        let before = tree.clone();
        let Err(TreeError::Full {
            capacity,
            requested,
        }) = tree.append_many(&[[3; 32]])
        else {
            panic!("a full tree rejects a leaf");
        };
        assert_eq!(
            (capacity, requested),
            (1 << SPROUT_DEPTH, (1 << SPROUT_DEPTH) + 1)
        );
        assert_eq!(tree, before);
        // The last free position takes one leaf and not two.
        let ommers = vec![SproutNode([1; 32]); SPROUT_DEPTH as usize - 1];
        let room = Frontier::from_parts(Position::from(last - 1), SproutNode([2; 32]), ommers)
            .expect("consistent parts");
        let mut tree = SproutFrontier::from_frontier(room);
        let Err(TreeError::Full { .. }) = tree.clone().append_many(&[[3; 32], [4; 32]]) else {
            panic!("two leaves do not fit");
        };
        tree.append_many(&[[3; 32]]).expect("one leaf fits");
    }
}
