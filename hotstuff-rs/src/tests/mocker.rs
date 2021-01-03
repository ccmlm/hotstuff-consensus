//! Mocker for testing.
use std::{mem::MaybeUninit, unimplemented, vec};
use std::collections::{HashMap};
use std::sync::Arc; 

use threshold_crypto::{PublicKeySet, SecretKeySet, SecretKeyShare, SignatureShare, Signature};
use log::{debug};
use simplelog::{CombinedLogger, WriteLogger,  LevelFilter, Config};


use crate::{msg::Context, safety::{basic::*, machine::{Machine, Ready, Safety, SafetyErr, SafetyEvent}, voter::Voter}, safety_storage::in_mem::InMemoryStorage};

pub(crate) fn init_logger(){
    let _ = CombinedLogger::init(
        vec![
            //TermLogger::new(LevelFilter::Debug, Config::default(), TerminalMode::Mixed),
            WriteLogger::new(LevelFilter::Debug, Config::default(), std::fs::File::create("./my_rust_bin.log").unwrap())
        ]
    );
}

pub(crate) fn threshold_sign_kit(
    n: usize,
    t: usize,
) -> (SecretKeySet, PublicKeySet, Vec<(usize, SecretKeyShare)>) {
    assert!(t <= n);

    let s = SecretKeySet::random(t, &mut rand::thread_rng());

    let vec_sk = (0..n).map(|i| (i, s.secret_key_share(i))).collect();
    let pks = s.public_keys();
    (s, pks, vec_sk)
}

pub enum ExpectedState<'a> {
    LockedInHeight(usize),
    CommittedBeforeHeight(usize),
    PropsalBaseOn(String, &'a TreeNode),
}

pub struct MockHotStuff {

    testee: Option<Machine<InMemoryStorage>>, 

    pks: Option<PublicKeySet>,
    sk: Option<SecretKeySet>,
    sks: Vec<(usize, SecretKeyShare)>,
    
    tx_to_hash: HashMap<String, NodeHash>, 

    nodes: HashMap<NodeHash, Arc<TreeNode>>, 
    qcs: HashMap<QCHash, Arc<GenericQC>>, 
    parent: NodeHash, 
    // 
    qc_high: Arc<GenericQC>, 
    height: u64, 
    th: usize,
    n: usize,
    leader_id: usize,
    testee_id: usize,
    init_done: bool, 
}

impl MockHotStuff {
    pub fn new(n: usize) -> Self {
        let init_node = Arc::new(TreeNode::genesis());
        let init_qc = Arc::new(GenericQC::genesis(0, &init_node));
        let init_node_hash = TreeNode::hash(&init_node); 
        // let init_qc_hash = GenericQC::hash(&init_qc); 

        Self {
            testee: None, 
            parent: init_node_hash, 
            qc_high: init_qc, 
            
            pks: None,
            sk: None,
            sks: Vec::new(),
            tx_to_hash: HashMap::new(), 

            nodes: HashMap::new(), 
            qcs: HashMap::new(), 

            height: 0, 
            th: 0,
            n,
            leader_id: 0,
            testee_id: 0,

            init_done: false, 
        }
    }

    pub fn specify_leader(&mut self, leader: usize) -> &mut Self {
        assert!(leader < self.n);
        self.leader_id = leader;
        self
    }

    pub fn specify_testee(&mut self, testee: usize) -> &mut Self {
        assert!(testee < self.n);
        self.testee_id = testee;
        self
    }

    /// Initialize state. 
    pub fn init(&mut self) {
        assert!(!self.init_done);
        assert!(self.n > 0);
        assert!(self.testee_id < self.n);

        self.th = (self.n << 1) / 3;
        let (a, b, c) = threshold_sign_kit(self.n, self.th);
        self.sk = Some(a);
        self.pks = Some(b);
        self.sks = c;


        let init_node = Arc::new(TreeNode::genesis());
        let init_qc = Arc::new(GenericQC::genesis(0, &init_node));
        let init_node_hash = TreeNode::hash(&init_node); 
        let init_qc_hash = GenericQC::hash(&init_qc); 

        self.tx_to_hash.insert(format!("init"), init_node_hash.clone());

        let view = 0;
        let mut node_pool  = HashMap::new();
        node_pool.insert(init_node_hash.clone(), init_node.clone()); 
        let mut qc_map = HashMap::new();
        qc_map.insert(init_qc_hash.clone(), init_qc.clone());

        self.parent = init_node_hash;
        self.qc_high = init_qc;
        
        let voter = Voter::new(self.testee_id, self.pks.as_ref().unwrap().clone(), self.sk.as_ref().unwrap().secret_key_share(self.testee_id));
        let storage = InMemoryStorage::new(node_pool, qc_map, view, &init_node, self.qc_high.as_ref());
        let testee: Machine<InMemoryStorage> = Machine::new(
            voter,
            format!("{}", self.testee_id), 
            self.n, 
            Some(format!("{}", self.leader_id)), 
            storage,
        );

        self.testee = Some(testee);
        self.init_done = true;
    }

    /// Construct chain for testee. Note that no qc of last proposal is formed!
    /// For example `a1 <-qc- a2 <-qc- a3`, a1 has 2 qc, a2 has one, but qc of a3 isn't formed. 
    pub fn load_continue_chain(&mut self, cmds: Vec<String>) -> &mut Self {
        // It's init_qc now 
        let mut prev_qc= self.qc_high.clone(); 
        let mut view = 0;
        for cmd in cmds{
            view += 1;
            // make node
            let (node, hash) = 
            TreeNode::node_and_hash(vec![&Txn::new(cmd.as_bytes())], view, &self.parent, &GenericQC::hash(self.qc_high.as_ref()));

            assert!(self.send_correct_proposal(node.as_ref(), prev_qc.as_ref()).is_ok());

            // make qc with signs from replicas, 
            prev_qc = self.form_qc(view, &node, &hash);
    
            // update state
            self.update(cmd, view, prev_qc.clone(), node, hash);
        }
        self
    }

    /// Assertion about internal state of hotstuff module. 
    /// Panic if assertion failed.  
    pub fn check_with_expected_state(&self, expected: &ExpectedState){
        let ss = self.testee.as_ref().unwrap().take_snapshot(); 
        match expected{
            ExpectedState::LockedInHeight(h) => {
                assert!(ss.locked_node_height == *h as u64, format!("{:?}", ss));
            }, 
            ExpectedState::CommittedBeforeHeight(h) => {
                assert!(ss.last_committed_height >= *h as u64, format!("{:?}", ss)); 
            }
            _ => unimplemented!(),
        }; 
    }

    /// let testee extends branch from specified parent`. This method will increate height. 
    /// Panic if parent didn't exist. 
    pub fn extend_from(&mut self, parent: String, tx: String) {
        let parent_hash = self.tx_to_hash.get(&parent).unwrap().clone();
        let parent = self.nodes.get(&parent_hash).unwrap().clone();
        // form qc of parent and use it to create new proposal. 
        let qc = self.form_qc(self.height, &parent, &parent_hash);

        let res = self.propose(&parent_hash, tx, qc);
        
        assert!(
            if let Ok(Ready::Signature(_, _, _)) = res{
                true
            }else{
                false
            }
        );
    }

    /// Send new proposal with corrupted qc to testee. This method will increate height. 
    pub fn propose_with_corrupted_qc(&mut self, parent: String, tx: String) {
        let parent_hash = self.tx_to_hash.get(&parent).unwrap().clone();

        // form qc of parent and use it to create new proposal. 
        let qc = self.form_corrupted_qc(self.height, &parent_hash);
        let res = self.propose(&parent_hash, tx, qc);

        assert!(
            if let Err(SafetyErr::CorruptedQC) = res{
                true
            }else{
                false
            }
        );
    }

    /// Leader recv txs from other 
    pub fn recv_new_view_msg(&mut self, num: usize, txs: Vec<String>) -> &mut Self{
        assert!(num >= 1 && num <= self.n);
        for (i, tx) in txs.iter().enumerate(){
            let node_hash = self.tx_to_hash.get(tx).unwrap(); 
            debug!("{:?}", &node_hash);
            let node = self.nodes.get(node_hash).unwrap();
            let qc = self.qcs.get(&node.justify).unwrap();
            let _ = self.testee.as_mut().unwrap().process_safety_event(
            SafetyEvent::RecvNewViewMsg(
                    Context{
                        from: format!("{}", i), 
                        view: self.height, 
                    }, 
                    node.clone(), 
                    qc.clone()
                ) 
            ).unwrap();
        }
        self
    }

    pub fn make_proposal(&mut self, new_tx: String) -> Ready{
        self.testee
        .as_mut()
        .unwrap()
        .process_safety_event(SafetyEvent::NewTx(vec![Txn::new(new_tx)]))
        .unwrap()
    }

    pub fn check_proposal_with(&self, expected: &ExpectedState) -> &Self{
        match expected{
            ExpectedState::PropsalBaseOn(parent, prop) => {
                let parent = self.tx_to_hash.get(parent).unwrap();
                assert_eq!(parent, &prop.parent);
            }, 
            _ => panic!("invalided expected state for this method"), 
        }

        self
    }

    fn propose(&mut self, parent_hash:&NodeHash, tx: String, qc: Arc<GenericQC>) -> Result<Ready, SafetyErr>{
        // parent <- qc <- new_node

        // form qc of parent and use it to create new proposal. 
        // let qc = self.form_qc(self.height, &parent_hash);
        let qc_hash = Arc::new(GenericQC::hash(qc.as_ref())); 
        let (node, hash) = TreeNode::node_and_hash(vec![&Txn::new(tx.as_bytes())], self.height + 1, parent_hash, &qc_hash);
        let res = self.send_correct_proposal(&node, &qc); 
        self.update(tx, self.height + 1, qc, node, hash);
        res
    }

    fn send_correct_proposal(&mut self, node: &TreeNode, justify: &GenericQC) -> Result<Ready, SafetyErr>{
        // send node & qc to the testee
        self.testee.as_mut().unwrap().on_recv_proposal(&Context{
            from: format!("mocker"), 
            view: self.height as u64,  
        }, node, justify)
    }

    /// Create quorum certificate for a node. 
    fn form_qc(&mut self, view: ViewNumber, node: &TreeNode, node_hash: &NodeHash) -> Arc<GenericQC>{
        // make qc with signs from replicas, 
        let node_bytes = node.to_be_bytes();
        let signs = self.sks
            .iter()
            .map(|(i, sk)| (*i, sk.sign(&node_bytes)))
            .collect::<Vec<(usize, SignatureShare)>>();
        let combined_sign = self.pks.as_ref().unwrap()
        .combine_signatures(signs.iter().map(|(i, s)| (*i, s)))
        .unwrap();

        Arc::new(GenericQC::new(view, node_hash, &combined_sign))
    }

    fn form_corrupted_qc(&mut self, view: ViewNumber, node_hash: &NodeHash) -> Arc<GenericQC>{
        
        let combined_sign: Signature = unsafe{
            MaybeUninit::uninit().assume_init()
        };

        Arc::new(GenericQC::new(view, node_hash, &combined_sign))
    }

    fn update(&mut self, cmd: String, view: ViewNumber, prev_qc:Arc<GenericQC>, node: Box<TreeNode>, hash: Box<NodeHash>){
        self.tx_to_hash.insert(cmd, hash.as_ref().clone());
        self.height = u64::max(self.height, view); 
        self.parent = *hash;
        self.qc_high = prev_qc.clone();
        self.nodes.insert(TreeNode::hash(node.as_ref()), Arc::new(*node));
        self.qcs.insert(GenericQC::hash(prev_qc.as_ref()), self.qc_high.clone());
    }

}
