HTTP_PORT_NAME = "http"
RPC_PORT_NAME = "rpc"
CONSENSUS_PORT_NAME = "consensus"
ENGINE_RPC_PORT_NAME = "engine-rpc"
WS_PORT_NAME = "ws"
INTEROP_RPC_PORT_NAME = "rpc-interop"
TCP_DISCOVERY_PORT_NAME = "tcp-discovery"
UDP_DISCOVERY_PORT_NAME = "udp-discovery"


# Creates a struct representing a service port configuration
#
# This is useful especially for testing purposes since the assertion library does not work great with PortSpec
# (or any custom object for that matter)
def port(number, transport_protocol="TCP", application_protocol="http"):
    return struct(
        number=number,
        transport_protocol=transport_protocol,
        application_protocol=application_protocol,
    )


# Converts port struct to a PortSpec object for service creation
def port_to_port_spec(port):
    return PortSpec(
        number=port.number,
        transport_protocol=port.transport_protocol,
        application_protocol=port.application_protocol,
    )


# Converts a dictionary of port objects into a dictionary of PortSpec objects
def ports_to_port_specs(ports_dict):
    return {k: port_to_port_spec(v) for k, v in ports_dict.items()}


# Creates a service URL (on the internal network) from service name and a port object
def service_url(service_name, service_port):
    return "{}://{}:{}".format(
        service_port.application_protocol, service_name, service_port.number
    )
