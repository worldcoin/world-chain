optimism_package = import_module("github.com/ethpandaops/optimism-package/main.star@5c6b3267345da8f9409da8ef9bb290cd5608a3ee")

world_chain_builder = import_module("./el/world_chain_builder_launcher.star")

rundler = import_module("./bundler/rundler/rundler_launcher.star")
rundler_static = import_module("./static_files/static_files.star")

def run(plan, args={}):
  optimism_package.run(plan, args, custom_launchers={
      "el_builder_launcher": {
        "launcher": world_chain_builder.new_op_reth_builder_launcher,
        "launch_method": world_chain_builder.launch,
      },
    }
  )

  rundler_builder_config_file = plan.upload_files(
      src=rundler_static.RUNDLER_BUILDER_CONFIG_FILE_PATH,
      name="builder_config.json",
  )
  rundler_mempool_config_file = plan.upload_files(
      src=rundler_static.RUNDLER_MEMPOOL_CONFIG_FILE_PATH,
      name="mempool_config.json",
  )
  rundler_chain_spec = plan.upload_files(
      src=rundler_static.RUNDLER_CHAIN_SPEC_FILE_PATH,
      name="chain_spec.json",
  )

  # Extract HTTP RPC url of the builder
  builder_srv = plan.get_service("op-el-builder-1-custom-op-node-op-kurtosis")
  builder_rpc_port = builder_srv.ports["rpc"].number
  builder_rpc_url = "http://{0}:{1}".format(builder_srv.ip_address, builder_rpc_port)

  plan.print(builder_rpc_url)

  rundler.launch(plan, 
      service_name="rundler", 
      image="alchemyplatform/rundler:v0.6.0-alpha.3",
      rpc_http_url=builder_rpc_url,
      builder_config_file=rundler_builder_config_file,
      mempool_config_file=rundler_mempool_config_file,
      chain_spec_file=rundler_chain_spec,
  )

