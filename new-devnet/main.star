optimism_package = import_module("github.com/dzejkop/optimism-package/main.star@5c6b3267345da8f9409da8ef9bb290cd5608a3ee")

world_chain_builder = import_module("./el/world_chain_builder_launcher.star")

def run(plan, args={}):
  optimism_package.run(plan, args, custom_launchers={
      "el_builder_launcher": {
        "launcher": world_chain_builder.new_op_reth_builder_launcher,
        "launch_method": world_chain_builder.launch,
      },
    }
  )
