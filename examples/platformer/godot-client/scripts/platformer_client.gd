extends Node

## Platformer NetClient Wrapper
## Uses Mokosh NetClient GDExtension to connect to server

signal connected_to_server
signal disconnected_from_server
signal game_state_received(state: Dictionary)

var client: Node = null
var server_url := "ws://127.0.0.1:8080"
var is_connected_to_server := false

func _ready() -> void:
	print("🎮 Platformer client ready")

	# Create NetClient from GDExtension
	if not ClassDB.class_exists("NetClient"):
		print("❌ NetClient not found! GDExtension not loaded.")
		return

	client = ClassDB.instantiate("NetClient")
	add_child(client)

	# Connect NetClient signals
	client.connected.connect(_on_net_client_connected)
	client.disconnected.connect(_on_net_client_disconnected)
	client.message_received.connect(_on_net_client_message)
	client.error_occurred.connect(_on_net_client_error)

	print("✅ NetClient created and signals connected")

func connect_to_server() -> void:
	if client:
		print("🔌 Connecting to: ", server_url)
		client.connect_to_server(server_url)

func send_input(move_x: float, jump: bool) -> void:
	if not is_connected_to_server or not client:
		return

	var input_data = {
		"move_x": move_x,
		"jump": jump
	}

	var json_string = JSON.stringify(input_data)
	client.send_message(json_string)

func disconnect_from_server() -> void:
	if client and is_connected_to_server:
		# Call NetClient's disconnect() method
		client.call("disconnect")

## NetClient signal handlers

func _on_net_client_connected() -> void:
	print("✅ Connected to server!")
	is_connected_to_server = true
	connected_to_server.emit()

func _on_net_client_disconnected(reason: String) -> void:
	print("❌ Disconnected: ", reason)
	is_connected_to_server = false
	disconnected_from_server.emit()

func _on_net_client_message(message: String) -> void:
	var json = JSON.new()
	var parse_result = json.parse(message)

	if parse_result == OK:
		var data = json.data
		if data is Dictionary and data.has("players"):
			game_state_received.emit(data)
	else:
		print("⚠️  Failed to parse JSON: ", message)

func _on_net_client_error(error: String) -> void:
	print("❌ NetClient error: ", error)
