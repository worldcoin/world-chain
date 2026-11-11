package integration_tests

import (
	"encoding/json"
	"fmt"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync/atomic"
	"testing"
	"time"

	"github.com/ethereum-optimism/infra/proxyd"
	"github.com/gorilla/mux"
	"github.com/gorilla/websocket"
	"github.com/stretchr/testify/require"
)

func TestWorldchainPreconfirmedLogsWS(t *testing.T) {
	for _, subscriptionID := range []string{`"subscription/opaque+01"`, `18446744073709551615`} {
		t.Run(subscriptionID, func(t *testing.T) {
			cursor := `"opaque-cursor/+=0001"`
			result := fmt.Sprintf(`{"version":1,"chainId":480,"status":"ready","view":null,"canonical":{"number":100,"hash":"0x%064x"},"events":[{"sequence":12,"kind":"progress","view":null,"logs":[]}],"cursor":%s,"hasMore":false,"viewAgeMs":25}`, 1, cursor)
			capabilityRequest := `{"jsonrpc":"2.0","id":"capabilities","method":"op_supportedCapabilities","params":[]}`
			capabilityResponse := `{"jsonrpc":"2.0","id":"capabilities","result":["worldchain_preconfirmed_logs_v1"]}`
			queryRequest := fmt.Sprintf(`{"jsonrpc":"2.0","id":1,"method":"worldchain_getPreconfirmedLogs","params":[{"topics":["0x%064x"]},{"cursor":%s,"limit":100}]}`, 2, cursor)
			queryResponse := fmt.Sprintf(`{"jsonrpc":"2.0","id":1,"result":%s}`, result)
			subscribeRequest := fmt.Sprintf(`{"jsonrpc":"2.0","id":2,"method":"worldchain_subscribe","params":["preconfirmedLogs",{},{"cursor":%s,"limit":100}]}`, cursor)
			subscribeResponse := fmt.Sprintf(`{"jsonrpc":"2.0","id":2,"result":%s}`, subscriptionID)
			update := fmt.Sprintf(`{"jsonrpc":"2.0","method":"worldchain_subscription","params":{"subscription":%s,"result":{"type":"update","result":%s}}}`, subscriptionID, result)
			unsubscribeRequest := fmt.Sprintf(`{"jsonrpc":"2.0","id":3,"method":"worldchain_unsubscribe","params":[%s]}`, subscriptionID)
			unsubscribeResponse := `{"jsonrpc":"2.0","id":3,"result":true}`
			terminalRequest := `{"jsonrpc":"2.0","id":4,"method":"worldchain_subscribe","params":["preconfirmedLogs",{}]}`
			terminalResponse := fmt.Sprintf(`{"jsonrpc":"2.0","id":4,"result":%s}`, subscriptionID)
			terminalError := fmt.Sprintf(`{"jsonrpc":"2.0","method":"worldchain_subscription","params":{"subscription":%s,"result":{"type":"error","code":-32051,"message":"event history expired","canonical":{"number":100,"hash":"0x%064x"}}}}`, subscriptionID, 1)
			responses := map[string][]string{
				capabilityRequest:  {capabilityResponse},
				queryRequest:       {queryResponse},
				subscribeRequest:   {subscribeResponse, update},
				unsubscribeRequest: {unsubscribeResponse},
				terminalRequest:    {terminalResponse, terminalError},
			}
			observed := make(chan string, 8)
			backendErrors := make(chan error, 8)
			var backendConnections atomic.Int32
			backend := NewMockWSBackend(func(*websocket.Conn) {
				backendConnections.Add(1)
			}, func(conn *websocket.Conn, messageType int, data []byte) {
				observed <- string(data)
				for _, response := range responses[string(data)] {
					if err := conn.SetWriteDeadline(time.Now().Add(2 * time.Second)); err != nil {
						backendErrors <- err
						return
					}
					if err := conn.WriteMessage(messageType, []byte(response)); err != nil {
						backendErrors <- err
						return
					}
				}
			}, nil)
			t.Cleanup(backend.Close)
			proxyURL := preconfirmedProxy(t, backend.URL(), []string{
				"op_supportedCapabilities", "worldchain_getPreconfirmedLogs",
				"worldchain_subscribe", "worldchain_unsubscribe",
			})
			dialer := websocket.Dialer{HandshakeTimeout: 2 * time.Second}
			for _, path := range []string{"", "/invalid-test-key"} {
				conn, response, err := dialer.Dial(proxyURL+path, nil)
				if conn != nil {
					conn.Close()
				}
				require.Error(t, err)
				require.NotNil(t, response)
				require.Equal(t, http.StatusUnauthorized, response.StatusCode)
				response.Body.Close()
			}
			require.Zero(t, backendConnections.Load(), "authorization must precede backend connection")
			conn, _, err := dialer.Dial(proxyURL+"/local-test-key", nil)
			require.NoError(t, err)
			t.Cleanup(func() { conn.Close() })
			for _, request := range []string{capabilityRequest, queryRequest, subscribeRequest, unsubscribeRequest, terminalRequest} {
				require.NoError(t, conn.SetWriteDeadline(time.Now().Add(2*time.Second)))
				require.NoError(t, conn.WriteMessage(websocket.TextMessage, []byte(request)))
				select {
				case got := <-observed:
					require.Equal(t, request, got, "request bytes must be forwarded unchanged")
				case err := <-backendErrors:
					t.Fatal(err)
				case <-time.After(2 * time.Second):
					t.Fatal("backend did not receive whitelisted request")
				}
				for _, expected := range responses[request] {
					require.NoError(t, conn.SetReadDeadline(time.Now().Add(2*time.Second)))
					messageType, got, err := conn.ReadMessage()
					require.NoError(t, err)
					require.Equal(t, websocket.TextMessage, messageType)
					require.Equal(t, expected, string(got), "response/notification bytes must be forwarded unchanged")
				}
			}
			require.EqualValues(t, 1, backendConnections.Load(), "one connection must remain bound to one backend")
		})
	}

	t.Run("method requires whitelist", func(t *testing.T) {
		observed := make(chan string, 1)
		backend := NewMockWSBackend(nil, func(_ *websocket.Conn, _ int, data []byte) {
			observed <- string(data)
		}, nil)
		t.Cleanup(backend.Close)
		proxyURL := preconfirmedProxy(t, backend.URL(), []string{"eth_subscribe"})
		dialer := websocket.Dialer{HandshakeTimeout: 2 * time.Second}
		conn, _, err := dialer.Dial(proxyURL+"/local-test-key", nil)
		require.NoError(t, err)
		t.Cleanup(func() { conn.Close() })
		require.NoError(t, conn.SetWriteDeadline(time.Now().Add(2*time.Second)))
		require.NoError(t, conn.WriteMessage(websocket.TextMessage, []byte(`{"jsonrpc":"2.0","id":5,"method":"worldchain_subscribe","params":["preconfirmedLogs",{}]}`)))
		require.NoError(t, conn.SetReadDeadline(time.Now().Add(2*time.Second)))
		_, got, err := conn.ReadMessage()
		require.NoError(t, err)
		var response struct {
			ID    int `json:"id"`
			Error struct {
				Code int `json:"code"`
			} `json:"error"`
		}
		require.NoError(t, json.Unmarshal(got, &response))
		require.Equal(t, 5, response.ID)
		require.Equal(t, -32601, response.Error.Code)
		select {
		case message := <-observed:
			t.Fatalf("unlisted method reached backend: %s", message)
		case <-time.After(50 * time.Millisecond):
		}
	})
}

func preconfirmedProxy(t *testing.T, backendURL string, whitelist []string) string {
	t.Helper()
	t.Setenv("GOOD_BACKEND_RPC_URL", backendURL)
	config := ReadConfig("ws")
	config.Server.RPCPort = 0
	config.Server.WSPort = 0
	config.Server.PublicAccess = false
	config.Authentication = map[string]string{"local-test-key": "local-test-client"}
	config.WSMethodWhitelist = whitelist
	server, shutdown, err := proxyd.Start(config)
	require.NoError(t, err)
	t.Cleanup(shutdown)
	router := mux.NewRouter()
	router.HandleFunc("/", server.HandleWS)
	router.HandleFunc("/{authorization}", server.HandleWS)
	frontend := httptest.NewServer(router)
	t.Cleanup(frontend.Close)
	return strings.Replace(frontend.URL, "http://", "ws://", 1)
}
