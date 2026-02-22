extends Control

## Client Demo for GodotNetLink
##
## Demonstrates how to use the NetClient GDExtension class to:
## - Connect to a server
## - Send and receive messages
## - Monitor connection status and RTT

# UI References
@onready var status_label := $VBoxContainer/StatusLabel
@onready var rtt_label := $VBoxContainer/RTTLabel
@onready var url_input := $VBoxContainer/URLInput
@onready var connect_button := $VBoxContainer/ConnectButton
@onready var disconnect_button := $VBoxContainer/DisconnectButton
@onready var message_input := $VBoxContainer/MessageInput
@onready var send_button := $VBoxContainer/SendButton
@onready var log_text := $VBoxContainer/LogText

# NetClient instance (added as child node)
var net_client: Node = null

func _ready() -> void:
	# Initialize UI
	disconnect_button.disabled = true
	send_button.disabled = true
	url_input.text = "ws://localhost:8080"

	# Create NetClient instance
	# Note: NetClient is a GDExtension class from the Rust library
	if ClassDB.class_exists("NetClient"):
		net_client = ClassDB.instantiate("NetClient")
		add_child(net_client)

		# Connect to NetClient signals
		net_client.connected.connect(_on_client_connected)
		net_client.disconnected.connect(_on_client_disconnected)
		net_client.message_received.connect(_on_message_received)
		net_client.error_occurred.connect(_on_error_occurred)

		log_message("NetClient initialized successfully")
		update_status("Not connected")
	else:
		log_message("ERROR: NetClient class not found! Make sure the GDExtension is loaded.")
		update_status("ERROR: NetClient not available")
		connect_button.disabled = true

	# Connect UI signals
	connect_button.pressed.connect(_on_connect_pressed)
	disconnect_button.pressed.connect(_on_disconnect_pressed)
	send_button.pressed.connect(_on_send_pressed)

func _process(_delta: float) -> void:
	# Update RTT display
	if net_client and net_client.is_connected():
		var rtt := net_client.get_rtt()
		if rtt >= 0:
			rtt_label.text = "RTT: %.1f ms" % rtt
		else:
			rtt_label.text = "RTT: N/A"
	else:
		rtt_label.text = "RTT: N/A"

## UI Callbacks

func _on_connect_pressed() -> void:
	var url := url_input.text.strip_edges()
	if url.is_empty():
		log_message("ERROR: URL is empty")
		return

	log_message("Connecting to %s..." % url)
	net_client.connect_to_server(url)

	connect_button.disabled = true
	url_input.editable = false

func _on_disconnect_pressed() -> void:
	log_message("Disconnecting...")
	net_client.disconnect()

func _on_send_pressed() -> void:
	var message_text := message_input.text.strip_edges()
	if message_text.is_empty():
		return

	# Create a JSON message
	var message := {
		"type": "player_input",
		"text": message_text,
		"timestamp": Time.get_unix_time_from_system()
	}

	var json_str := JSON.stringify(message)
	log_message("Sending: %s" % json_str)
	net_client.send_message(json_str)

	message_input.clear()

## NetClient Signal Handlers

func _on_client_connected() -> void:
	log_message("Connected to server!")
	update_status("Connected")

	connect_button.disabled = true
	disconnect_button.disabled = false
	send_button.disabled = false
	url_input.editable = false

func _on_client_disconnected(reason: String) -> void:
	log_message("Disconnected: %s" % reason)
	update_status("Disconnected")

	connect_button.disabled = false
	disconnect_button.disabled = true
	send_button.disabled = true
	url_input.editable = true

func _on_message_received(message: String) -> void:
	log_message("Received: %s" % message)

	# Try to parse as JSON
	var json := JSON.new()
	var error := json.parse(message)
	if error == OK:
		var data: Dictionary = json.data
		log_message("  Parsed JSON: %s" % str(data))

func _on_error_occurred(error: String) -> void:
	log_message("ERROR: %s" % error)
	update_status("Error: %s" % error)

## Helper Functions

func update_status(status: String) -> void:
	status_label.text = "Status: %s" % status

func log_message(message: String) -> void:
	var timestamp := Time.get_time_string_from_system()
	var log_line := "[%s] %s\n" % [timestamp, message]
	log_text.text += log_line

	# Auto-scroll to bottom
	await get_tree().process_frame
	log_text.scroll_vertical = log_text.get_v_scroll_bar().max_value
