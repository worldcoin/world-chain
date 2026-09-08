use alloy_sol_types::sol;

sol! {
    #[sol(rpc)]
    interface IFaultDisputeGame {
        /// @notice The current status of the dispute game.
        #[derive(Debug, PartialEq)]
        enum GameStatus {
            // The game is currently in progress, and has not been resolved.
            IN_PROGRESS,
            // The game has concluded, and the `rootClaim` was challenged successfully.
            CHALLENGER_WINS,
            // The game has concluded, and the `rootClaim` could not be contested.
            DEFENDER_WINS
        }

        function status() external view returns (GameStatus);
    }
}
