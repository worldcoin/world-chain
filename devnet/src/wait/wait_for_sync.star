utils = import_module("../util.star")


def wait_for_sync(plan, l1_config_env_vars):
    plan.run_sh(
        name="wait-for-l1-sync",
        description="Wait for L1 to sync up to network - this can take up to 3days",
        env_vars=l1_config_env_vars,
        run='while true; do sleep 5; \
            current_head=$(curl -s $CL_RPC_URL/eth/v1/node/syncing | jq -r \'.data.head_slot\'); \
            sync_distance=$(curl -s $CL_RPC_URL/eth/v1/node/syncing | jq -r \'.data.sync_distance\'); \
            is_optimistic=$(curl -s $CL_RPC_URL/eth/v1/node/syncing | jq -r \'.data.is_optimistic\'); \
            el_sync=$(curl -s -X POST -H "Content-Type: application/json" -d \'{"jsonrpc":"2.0","method":"eth_syncing","params":[],"id":1}\' $L1_RPC_URL | jq -r \'.result\'); \
            if [ "$el_sync" == "false" ]; then echo \'Node is starting up\'; else \
            el_current_block_hex=$(echo $el_sync | jq -r \'.currentBlock\'); \
            el_highest_block_hex=$(echo $el_sync | jq -r \'.highestBlock\'); \
            el_current_block=$(printf %d $el_current_block_hex); \
            el_highest_block=$(printf %d $el_highest_block_hex); \
            number_of_blocks_left=$(($current_head - $el_highest_block)); \
            fi; \
            echo "Your L1 is still syncing. Current CL head is $current_head and CL sync distance is $sync_distance. EL current head is: $el_current_block and highest block is: $el_highest_block. Number of blocks left ~$number_of_blocks_left"; \
            if [ "$is_optimistic" == "false" ]; then echo \'Node is synced!\'; break; fi; done',
        wait="72h",
    )


def wait_for_startup(plan, l1_config_env_vars):
    plan.run_sh(
        name="wait-for-l1-consensus-startup",
        description="Wait for L1 to start up - can take up to 2 minutes",
        image=utils.DEPLOYMENT_UTILS_IMAGE,
        env_vars=l1_config_env_vars,
        run="while true; do sleep 5; echo 'L1 Chain is starting up'; if [ \"$(curl -s $CL_RPC_URL/eth/v1/beacon/headers/ | jq -r '.data[0].header.message.slot')\" != \"0\" ]; then echo 'L1 Chain has started!'; break; fi; done",
        wait="300s",
    )

    # wait for block 3 to avoid transaction indexing in progress errors
    plan.run_sh(
        name="wait-for-l1-execution-startup",
        description="Wait for L1 execution to start up - can take up to 2 minutes",
        image=utils.DEPLOYMENT_UTILS_IMAGE,
        env_vars=l1_config_env_vars,
        run='while true; do sleep 5; current_head=$(cast bn --rpc-url=$L1_RPC_URL); echo "L1 Execution is starting up"; if [ "$current_head" -ge "3" ]; then echo "L1 Execution has started!"; break; fi; done',
        wait="5m",
    )
