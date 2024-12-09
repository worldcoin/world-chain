ENVRC_PATH = "/workspace/optimism/.envrc"
FACTORY_DEPLOYER_ADDRESS = "0x3fAB184622Dc19b6109349B94811493BF2a45362"
FACTORY_ADDRESS = "0x4e59b44847b379578588920cA78FbF26c0B4956C"
# raw tx data for deploying Create2Factory contract to L1
FACTORY_DEPLOYER_CODE = "0xf8a58085174876e800830186a08080b853604580600e600039806000f350fe7fffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffe03601600081602082378035828234f58015156039578182fd5b8082525050506014600cf31ba02222222222222222222222222222222222222222222222222222222222222222a02222222222222222222222222222222222222222222222222222222222222222"

CHAINSPEC_JQ_FILEPATH = "../../static_files/chainspec_template/gen2spec.jq"


def deploy_factory_contract(
    plan,
    priv_key,
    l1_config_env_vars,
    image,
):
    factory_deployment_result = plan.run_sh(
        name="op-deploy-factory-contract",
        description="Deploying L2 factory contract to L1 (takes about a minute)",
        image=image,
        env_vars={
            "PRIVATE_KEY": str(priv_key),
            "FUND_VALUE": "10ether",
            "DEPLOY_CONFIG_PATH": "/workspace/optimism/packages/contracts-bedrock/deploy-config/getting-started.json",
            "DEPLOYMENT_CONTEXT": "getting-started",
        }
        | l1_config_env_vars,
        run=" && ".join(
            [
                "while true; do sleep 1; echo 'L1 Chain is starting up'; if [ \"$(curl -s $CL_RPC_URL/eth/v1/beacon/headers/ | jq -r '.data[0].header.message.slot')\" != \"0\" ]; then echo 'L1 Chain has started!'; break; fi; done",
                "cast send {0} --value $FUND_VALUE --rpc-url $L1_RPC_URL --private-key $PRIVATE_KEY".format(
                    FACTORY_DEPLOYER_ADDRESS
                ),
                "if [ $(cast codesize {0} --rpc-url $L1_RPC_URL) -gt 0 ]; then echo 'Factory contract already deployed!'; exit 0; fi".format(
                    FACTORY_ADDRESS
                ),
                "cast publish --rpc-url $L1_RPC_URL {0}".format(FACTORY_DEPLOYER_CODE),
            ]
        ),
        wait="300s",
    )


def deploy_l2_contracts(
    plan,
    priv_key,
    l1_config_env_vars,
    l2_config_env_vars,
    l2_services_suffix,
    fork_activation_env,
    image,
):
    chainspec_files_artifact = plan.upload_files(
        src=CHAINSPEC_JQ_FILEPATH,
        name="op-chainspec-config{0}".format(l2_services_suffix),
    )

    op_genesis = plan.run_sh(
        name="op-deploy-l2-contracts",
        description="Deploying L2 contracts (takes about a minute)",
        image=image,
        env_vars={
            "PRIVATE_KEY": str(priv_key),
            "FUND_VALUE": "10ether",
            "DEPLOY_CONFIG_PATH": "/workspace/optimism/packages/contracts-bedrock/deploy-config/getting-started.json",
            "DEPLOYMENT_CONTEXT": "getting-started",
        }
        | l1_config_env_vars
        | l2_config_env_vars
        | fork_activation_env,
        files={
            "/workspace/optimism/packages/contracts-bedrock/deploy-config/chainspec-generator/": chainspec_files_artifact,
        },
        store=[
            StoreSpec(
                src="/network-configs",
                name="op-genesis-configs{0}".format(l2_services_suffix),
            ),
        ],
        run=" && ".join(
            [
                "./packages/contracts-bedrock/scripts/getting-started/wallets.sh >> {0}".format(
                    ENVRC_PATH
                ),
                "echo 'export IMPL_SALT=$(openssl rand -hex 32)' >> {0}".format(
                    ENVRC_PATH
                ),
                ". {0}".format(ENVRC_PATH),
                "mkdir -p /network-configs",
                "cast send $GS_ADMIN_ADDRESS --value $FUND_VALUE --private-key $PRIVATE_KEY --rpc-url $L1_RPC_URL",  # Fund Admin
                "cast send $GS_BATCHER_ADDRESS --value $FUND_VALUE --private-key $PRIVATE_KEY --rpc-url $L1_RPC_URL",  # Fund Batcher
                "cast send $GS_PROPOSER_ADDRESS --value $FUND_VALUE --private-key $PRIVATE_KEY --rpc-url $L1_RPC_URL",  # Fund Proposer
                "cd /workspace/optimism/packages/contracts-bedrock",
                "./scripts/getting-started/config.sh",
                'jq \'. + {"fundDevAccounts": true, "useInterop": true}\' $DEPLOY_CONFIG_PATH > tmp.$$.json && mv tmp.$$.json $DEPLOY_CONFIG_PATH',
                "forge script scripts/deploy/Deploy.s.sol:Deploy --private-key $GS_ADMIN_PRIVATE_KEY --broadcast --rpc-url $L1_RPC_URL",
                "CONTRACT_ADDRESSES_PATH=$DEPLOYMENT_OUTFILE forge script scripts/L2Genesis.s.sol:L2Genesis --sig 'runWithStateDump()' --chain-id $L2_CHAIN_ID",
                'cat $STATE_DUMP_PATH | jq \'. |= . + {"0x4e59b44847b379578588920ca78fbf26c0b4956c": {"nonce": "0x0","balance": "0x0","code": "0x7fffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffe03601600081602082378035828234f58015156039578182fd5b8082525050506014600cf3","storage": {}},"0x047eE5313F98E26Cc8177fA38877cB36292D2364": {"nonce": "0x2","balance": "0x0","code": "0x608060405234801561001057600080fd5b50600436106100d45760003560e01c8063b242e53411610081578063f1c621ee1161005b578063f1c621ee146101c9578063f2fde38b1461021c578063fbde929b1461022f57600080fd5b8063b242e5341461019b578063c70aa727146101ae578063d7b0fef1146101c157600080fd5b80638da5cb5b116100b25780638da5cb5b146101305780638e5cdd5014610158578063b0d690791461018957600080fd5b80630ee04629146100d9578063354ca12014610113578063715018a614610128575b600080fd5b6004546100fe9074010000000000000000000000000000000000000000900460ff1681565b60405190151581526020015b60405180910390f35b610126610121366004610afe565b610242565b005b6101266102f9565b60045460405173ffffffffffffffffffffffffffffffffffffffff909116815260200161010a565b60405160ff7f000000000000000000000000000000000000000000000000000000000000001e16815260200161010a565b6000545b60405190815260200161010a565b6101266101a9366004610b73565b61030d565b6101266101bc366004610bb1565b61048c565b61018d6104a0565b6101fb6101d7366004610bb1565b6002602052600090815260409020546fffffffffffffffffffffffffffffffff1681565b6040516fffffffffffffffffffffffffffffffff909116815260200161010a565b61012661022a366004610bca565b6104e5565b61012661023d366004610bb1565b610599565b61024b856105d1565b60035460408051608081018252878152602081018690528082018790526060810185905290517f2357251100000000000000000000000000000000000000000000000000000000815273ffffffffffffffffffffffffffffffffffffffff909216916323572511916102c291859190600401610bee565b60006040518083038186803b1580156102da57600080fd5b505afa1580156102ee573d6000803e3d6000fd5b505050505050505050565b610301610693565b61030b6000610964565b565b610315610693565b73ffffffffffffffffffffffffffffffffffffffff82166103bd576040517f08c379a000000000000000000000000000000000000000000000000000000000815260206004820152603260248201527f43726f7373446f6d61696e4f776e61626c65333a206e6577206f776e6572206960448201527f7320746865207a65726f2061646472657373000000000000000000000000000060648201526084015b60405180910390fd5b60006103de60045473ffffffffffffffffffffffffffffffffffffffff1690565b90506103e983610964565b6004805483151574010000000000000000000000000000000000000000027fffffffffffffffffffffff00ffffffffffffffffffffffffffffffffffffffff90911617905560405173ffffffffffffffffffffffffffffffffffffffff80851691908316907f7fdc2a4b6eb39ec3363d710d188620bd1e97b3c434161f187b4d0dc0544faa589061047f90861515815260200190565b60405180910390a3505050565b610494610693565b61049d816109db565b50565b60006001546000036104de576040517f5b8dabb700000000000000000000000000000000000000000000000000000000815260040160405180910390fd5b5060015490565b6104ed610693565b73ffffffffffffffffffffffffffffffffffffffff8116610590576040517f08c379a000000000000000000000000000000000000000000000000000000000815260206004820152602660248201527f4f776e61626c653a206e6577206f776e657220697320746865207a65726f206160448201527f646472657373000000000000000000000000000000000000000000000000000060648201526084016103b4565b61049d81610964565b6105a1610693565b61049d81610a16565b60006010602060ff841682118015906105c957508060ff168460ff1611155b949350505050565b60015481036105dd5750565b6000818152600260205260408120546fffffffffffffffffffffffffffffffff1690819003610638576040517fddae3b7100000000000000000000000000000000000000000000000000000000815260040160405180910390fd5b6000546106576fffffffffffffffffffffffffffffffff831642610c2e565b111561068f576040517f3ae7359e00000000000000000000000000000000000000000000000000000000815260040160405180910390fd5b5050565b60045474010000000000000000000000000000000000000000900460ff161561077957336106d660045473ffffffffffffffffffffffffffffffffffffffff1690565b73ffffffffffffffffffffffffffffffffffffffff161461030b576040517f08c379a000000000000000000000000000000000000000000000000000000000815260206004820152602c60248201527f43726f7373446f6d61696e4f776e61626c65333a2063616c6c6572206973206e60448201527f6f7420746865206f776e6572000000000000000000000000000000000000000060648201526084016103b4565b73420000000000000000000000000000000000000733811461081d576040517f08c379a000000000000000000000000000000000000000000000000000000000815260206004820152603060248201527f43726f7373446f6d61696e4f776e61626c65333a2063616c6c6572206973206e60448201527f6f7420746865206d657373656e6765720000000000000000000000000000000060648201526084016103b4565b8073ffffffffffffffffffffffffffffffffffffffff16636e296e456040518163ffffffff1660e01b8152600401602060405180830381865afa158015610868573d6000803e3d6000fd5b505050506040513d601f19601f8201168201806040525081019061088c9190610c6c565b73ffffffffffffffffffffffffffffffffffffffff166108c160045473ffffffffffffffffffffffffffffffffffffffff1690565b73ffffffffffffffffffffffffffffffffffffffff161461049d576040517f08c379a000000000000000000000000000000000000000000000000000000000815260206004820152602c60248201527f43726f7373446f6d61696e4f776e61626c65333a2063616c6c6572206973206e60448201527f6f7420746865206f776e6572000000000000000000000000000000000000000060648201526084016103b4565b6004805473ffffffffffffffffffffffffffffffffffffffff8381167fffffffffffffffffffffffff0000000000000000000000000000000000000000831681179093556040519116919082907f8be0079c531659141344cd1fd0a4f28419497f9722a3daafe3b4186f6b6457e090600090a35050565b60008190556040518181527f147b815b6a3a8dd5d49310410e089f6b5e9f3782e944772edc938c8bb48ef1219060200160405180910390a150565b6000818152600260205260409020546fffffffffffffffffffffffffffffffff168015610a6f576040517f6650c4d100000000000000000000000000000000000000000000000000000000815260040160405180910390fd5b600182905560008281526002602090815260409182902080547fffffffffffffffffffffffffffffffff0000000000000000000000000000000016426fffffffffffffffffffffffffffffffff8116918217909255835186815292830152917fe97c89cbb137505b36f55ebfc9732fd6c4c73ff43d49db239fc25f6e7a534145910160405180910390a1505050565b6000806000806000610180808789031215610b1857600080fd5b86359550602087013594506040870135935060608701359250878188011115610b4057600080fd5b506080860190509295509295909350565b73ffffffffffffffffffffffffffffffffffffffff8116811461049d57600080fd5b60008060408385031215610b8657600080fd5b8235610b9181610b51565b915060208301358015158114610ba657600080fd5b809150509250929050565b600060208284031215610bc357600080fd5b5035919050565b600060208284031215610bdc57600080fd5b8135610be781610b51565b9392505050565b610180810161010080858437600090830181815284915b6004811015610c24578251825260209283019290910190600101610c05565b5050509392505050565b600082821015610c67577f4e487b7100000000000000000000000000000000000000000000000000000000600052601160045260246000fd5b500390565b600060208284031215610c7e57600080fd5b8151610be781610b5156fea164736f6c634300080f000a","storage": {"0x0000000000000000000000000000000000000000000000000000000000000000": "0x0000000000000000000000000000000000000000000000000000000000093a80","0x0000000000000000000000000000000000000000000000000000000000000002": "0x0AC9D5995841E9D09387685C7155E6F08B2A92D984A8A2F282C48D6CC08DB3CE","0x0000000000000000000000000000000000000000000000000000000000000003": "0x000000000000000000000000a16e02e87b7454126e5e10d957a927a7f5b5d2be","0x0000000000000000000000000000000000000000000000000000000000000004": "0x000000000000000000000001f39fd6e51aad88f6f4ce6ab8827279cfffb92266"}}}\' > tmp.$$.json && mv tmp.$$.json $STATE_DUMP_PATH',
                "cd /workspace/optimism/op-node/bin",
                "./op-node genesis l2 \
                    --l1-rpc $L1_RPC_URL \
                    --deploy-config $DEPLOY_CONFIG_PATH \
                    --l2-allocs $STATE_DUMP_PATH \
                    --l1-deployments $DEPLOYMENT_OUTFILE \
                    --outfile.l2 /network-configs/genesis.json \
                    --outfile.rollup /network-configs/rollup.json",
                "mv $DEPLOY_CONFIG_PATH /network-configs/getting-started.json",
                "mv $DEPLOYMENT_OUTFILE /network-configs/kurtosis.json",
                "mv $STATE_DUMP_PATH /network-configs/state-dump.json",
                "echo -n $GS_SEQUENCER_PRIVATE_KEY > /network-configs/GS_SEQUENCER_PRIVATE_KEY",
                "echo -n $GS_BATCHER_PRIVATE_KEY > /network-configs/GS_BATCHER_PRIVATE_KEY",
                "echo -n $GS_PROPOSER_PRIVATE_KEY > /network-configs/GS_PROPOSER_PRIVATE_KEY",
                "cat /network-configs/genesis.json | jq --from-file /workspace/optimism/packages/contracts-bedrock/deploy-config/chainspec-generator/gen2spec.jq > /network-configs/chainspec.json",
            ]
        ),
        wait="300s",
    )

    gs_sequencer_private_key = plan.run_sh(
        name="read-gs-sequencer-private-key",
        description="Getting the sequencer private key",
        run="cat /network-configs/GS_SEQUENCER_PRIVATE_KEY ",
        files={"/network-configs": op_genesis.files_artifacts[0]},
    )

    gs_batcher_private_key = plan.run_sh(
        name="read-gs-batcher-private-key",
        description="Getting the batcher private key",
        run="cat /network-configs/GS_BATCHER_PRIVATE_KEY ",
        files={"/network-configs": op_genesis.files_artifacts[0]},
    )

    gs_proposer_private_key = plan.run_sh(
        name="read-gs-proposer-private-key",
        description="Getting the proposer private key",
        run="cat /network-configs/GS_PROPOSER_PRIVATE_KEY ",
        files={"/network-configs": op_genesis.files_artifacts[0]},
    )

    l2oo_address = plan.run_sh(
        name="read-l2oo-address",
        description="Getting the L2OutputOracleProxy address",
        run="jq -r .L2OutputOracleProxy /network-configs/kurtosis.json | tr -d '\n'",
        files={"/network-configs": op_genesis.files_artifacts[0]},
    )

    l1_bridge_address = plan.run_sh(
        name="read-l1-bridge-address",
        description="Getting the L1StandardBridgeProxy address",
        run="jq -r .L1StandardBridgeProxy /network-configs/kurtosis.json | tr -d '\n'",
        files={"/network-configs": op_genesis.files_artifacts[0]},
    )

    l1_deposit_start_block = plan.run_sh(
        name="read-l1-deposit-start-block",
        description="Getting the L1StandardBridgeProxy address",
        image="badouralix/curl-jq",
        run="jq -r .genesis.l1.number  /network-configs/rollup.json | tr -d '\n'",
        files={"/network-configs": op_genesis.files_artifacts[0]},
    )

    l1_portal_contract = plan.run_sh(
        name="read-l1-portal-contract",
        description="Getting the L1 portal contract",
        run="jq -r .OptimismPortal  /network-configs/kurtosis.json | tr -d '\n'",
        files={"/network-configs": op_genesis.files_artifacts[0]},
    )

    private_keys = {
        "GS_SEQUENCER_PRIVATE_KEY": gs_sequencer_private_key.output,
        "GS_BATCHER_PRIVATE_KEY": gs_batcher_private_key.output,
        "GS_PROPOSER_PRIVATE_KEY": gs_proposer_private_key.output,
    }

    blockscout_env_variables = {
        "INDEXER_OPTIMISM_L1_PORTAL_CONTRACT": l1_portal_contract.output,
        "INDEXER_OPTIMISM_L1_DEPOSITS_START_BLOCK": l1_deposit_start_block.output,
        "INDEXER_OPTIMISM_L1_WITHDRAWALS_START_BLOCK": l1_deposit_start_block.output,
        "INDEXER_OPTIMISM_L1_BATCH_START_BLOCK": l1_deposit_start_block.output,
        "INDEXER_OPTIMISM_L1_OUTPUT_ORACLE_CONTRACT": l2oo_address.output,
    }

    return (
        op_genesis.files_artifacts[0],
        private_keys,
        l2oo_address.output,
        l1_bridge_address.output,
        blockscout_env_variables,
    )
