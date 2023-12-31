// This file is part of Astar.

// Copyright (C) 2019-2023 Stake Technologies Pte.Ltd.
// SPDX-License-Identifier: GPL-3.0-or-later

// Astar is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

// Astar is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.

// You should have received a copy of the GNU General Public License
// along with Astar. If not, see <http://www.gnu.org/licenses/>.

use crate::mocks::{
    msg_queue::mock_msg_queue,
    parachain::{self, System},
    *,
};

use frame_support::{assert_ok, weights::Weight};
use pallet_contracts::Determinism;
use parity_scale_codec::{Decode, Encode};
use sp_runtime::traits::Bounded;
use xcm::{prelude::*, v3::Response};
use xcm_simulator::TestExt;

const GAS_LIMIT: Weight = Weight::from_parts(100_000_000_000, 3 * 1024 * 1024);

#[test]
fn basic_xcmp_transact_outcome_query_response() {
    MockNet::reset();

    // basic remark call
    let remark = parachain::RuntimeCall::System(
        frame_system::Call::<parachain::Runtime>::remark_with_event {
            remark: vec![1, 2, 3],
        },
    );

    // priveleged root call
    let root_call =
        parachain::RuntimeCall::System(frame_system::Call::<parachain::Runtime>::set_storage {
            items: vec![(vec![0], vec![1])],
        });

    // Closure for sending Transact(call) expecting success to dest returning
    // query id for response
    let send_transact = |call: parachain::RuntimeCall, dest: MultiLocation| {
        // this will register the query and add `SetApendix` with `ReportError`.
        let query_id = ParachainPalletXcm::new_query(dest, Bounded::max_value(), Here);

        // build xcm message
        let xcm = Xcm(vec![
            WithdrawAsset((Here, 100_000_000_000_u128).into()),
            BuyExecution {
                fees: (Here, 100_000_000_000_u128).into(),
                weight_limit: Unlimited,
            },
            SetAppendix(Xcm(vec![ReportError(QueryResponseInfo {
                destination: (Parent, Parachain(1)).into(),
                query_id,
                max_weight: Weight::zero(),
            })])),
            Transact {
                origin_kind: OriginKind::SovereignAccount,
                require_weight_at_most: Weight::from_parts(1_000_000_000, 1024 * 1024),
                call: call.encode().into(),
            },
            ExpectTransactStatus(MaybeErrorCode::Success),
        ]);

        // send the XCM to ParaB
        assert_ok!(ParachainPalletXcm::send_xcm(Here, dest, xcm,));
        query_id
    };

    // send the remark Transct to ParaB expecting success and have outcome back
    let mut query_id_success = 999u64;
    ParaA::execute_with(|| {
        query_id_success = send_transact(remark, (Parent, Parachain(2)).into());
    });

    // check for if remark was executed in ParaB
    ParaB::execute_with(|| {
        use parachain::{RuntimeEvent, System};
        // check remark events
        assert!(System::events().iter().any(|r| matches!(
            r.event,
            RuntimeEvent::System(frame_system::Event::Remarked { .. })
        )));

        // clear the events
        System::reset_events();
    });

    // check the outcome we recieved from ParaB
    ParaA::execute_with(move || {
        let xcms = parachain::MsgQueue::received_xcmp();
        // sanity check
        assert!(
            xcms.len() == 1,
            "Expected only one XCMP message, found {}",
            xcms.len()
        );
        assert!(
            xcms[0].len() == 1,
            "Response XCM should only have one instruction, i.e QueryResponse, found {}",
            xcms[0].len()
        );
        assert!(matches!(
            xcms[0].0.as_slice(),
            &[QueryResponse {
                query_id,
                response: Response::ExecutionResult(None),
                ..
            }] if query_id == query_id_success
        ));

        // clear the events
        System::reset_events();
    });

    //
    // Failure
    //

    // send the root_call Transct to ParaB expecting failure and have outcome back
    let mut query_id_failure = 999u64;
    ParaA::execute_with(|| {
        query_id_failure = send_transact(root_call, (Parent, Parachain(2)).into());
    });

    // check for if remark was executed in ParaB
    ParaB::execute_with(|| {
        use parachain::{RuntimeEvent, System};
        // check queue failed events
        assert!(System::events().iter().any(|r| matches!(
            r.event,
            RuntimeEvent::MsgQueue(mock_msg_queue::Event::Fail(..))
        )));
    });

    // check the outcome we recieved from ParaB
    ParaA::execute_with(|| {
        let xcms = parachain::MsgQueue::received_xcmp();
        // sanity check
        assert!(xcms.len() == 2 && xcms[1].len() == 1);
        assert!(matches!(
            xcms[1].0.as_slice(),
            &[QueryResponse {
                query_id,
                response: Response::ExecutionResult(Some((4, xcm::v3::Error::ExpectationFalse))),
                ..
            }] if query_id == query_id_failure
        ));
    });
}

#[test]
fn xcm_remote_transact_contract() {
    MockNet::reset();

    const SELECTOR_CONSTRUCTOR: [u8; 4] = [0x9b, 0xae, 0x9d, 0x5e];
    const SELECTOR_GET: [u8; 4] = [0x2f, 0x86, 0x5b, 0xd9];
    const SELECTOR_FLIP: [u8; 4] = [0x63, 0x3a, 0xa5, 0x51];

    // deploy and initialize flipper contract with `true` in ParaA
    let mut contract_id = [0u8; 32].into();
    ParaA::execute_with(|| {
        (contract_id, _) = deploy_contract::<parachain::Runtime>(
            "flipper",
            ALICE.into(),
            0,
            GAS_LIMIT,
            None,
            // selector + true
            [SELECTOR_CONSTRUCTOR.to_vec(), vec![0x01]].concat(),
        );

        // check for flip status
        let outcome = ParachainContracts::bare_call(
            ALICE.into(),
            contract_id.clone(),
            0,
            GAS_LIMIT,
            None,
            SELECTOR_GET.to_vec(),
            true,
            Determinism::Deterministic,
        );
        let res = outcome.result.unwrap();
        // check for revert
        assert!(!res.did_revert());
        // decode the return value
        let flag = Result::<bool, ()>::decode(&mut res.data.as_ref()).unwrap();
        assert_eq!(flag, Ok(true));
    });

    ParaB::execute_with(|| {
        // dispatch call to flip contract
        let call = parachain::RuntimeCall::Contracts(pallet_contracts::Call::call {
            dest: contract_id.clone(),
            value: 0,
            gas_limit: Weight::from_parts(100_000_000_000, 1024 * 1024),
            storage_deposit_limit: None,
            data: SELECTOR_FLIP.to_vec(),
        });

        let xcm: Xcm<()> = Xcm(vec![
            WithdrawAsset((Here, INITIAL_BALANCE).into()),
            BuyExecution {
                fees: (Here, INITIAL_BALANCE).into(),
                weight_limit: Unlimited,
            },
            Transact {
                origin_kind: OriginKind::SovereignAccount,
                require_weight_at_most: Weight::from_parts(100_000_000_000_000, 1024 * 1024 * 1024),
                call: call.encode().into(),
            },
            ExpectTransactStatus(MaybeErrorCode::Success),
        ]);

        // send the XCM to ParaA
        assert_ok!(ParachainPalletXcm::send(
            parachain::RuntimeOrigin::signed(ALICE),
            Box::new((Parent, Parachain(1)).into()),
            Box::new(VersionedXcm::V3(xcm)),
        ));
    });

    // check for flip status, it should be false
    ParaA::execute_with(|| {
        let outcome = ParachainContracts::bare_call(
            ALICE.into(),
            contract_id.clone(),
            0,
            GAS_LIMIT,
            None,
            SELECTOR_GET.to_vec(),
            true,
            Determinism::Deterministic,
        );
        let res = outcome.result.unwrap();
        // check for revert
        assert!(res.did_revert() == false);
        // decode the return value, it should be false
        let flag = Result::<bool, ()>::decode(&mut res.data.as_ref()).unwrap();
        assert_eq!(flag, Ok(false));
    });
}
