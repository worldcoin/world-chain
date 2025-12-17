# TODO
- Create Standard block builder that simply pre-loads receipts, transactions, and gas used into the executor. Should flatten the reverts on finish()
- Transform the payload builder to initialize a standard payload builder is `bal_enabled == flase`
- Transform the execution coordinator to use a serial transaction validator if an access list does not exist on the Execution payload diff