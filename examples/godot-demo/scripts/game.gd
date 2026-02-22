extends Node2D

## Simple Multiplayer Game
##
## Demonstrates real-time player synchronization via GodotNetLink NetClient

# NetClient (GDExtension from Rust)
var client: Node = null
var my_client_id := ""

# Players dictionary: {client_id: Player node}
var players := {}

# Player scene
const PLAYER_SCENE = preload("res://scenes/player.tscn")

# Network config
const SERVER_URL = "ws://127.0.0.1:8080"
const POSITION_SEND_INTERVAL = 0.033  # Send position ~30 times per second

# Position throttling
var position_send_timer := 0.0
var last_sent_position := Vector2.ZERO

# UI
@onready var status_label := $UI/StatusLabel
@onready var player_count_label := $UI/PlayerCountLabel

func _ready() -> void:
	print("🎮 Starting game...")
	print("🔌 Using NetClient GDExtension")

	# Create NetClient from GDExtension
	if not ClassDB.class_exists("NetClient"):
		print("❌ NetClient not found! GDExtension not loaded.")
		status_label.text = "NetClient not available!"
		status_label.modulate = Color.RED
		return

	client = ClassDB.instantiate("NetClient")
	add_child(client)

	print("✅ NetClient created")

	# Connect signals
	client.connected.connect(_on_client_connected)
	client.disconnected.connect(_on_client_disconnected)
	client.message_received.connect(_on_client_message)
	client.error_occurred.connect(_on_client_error)

	print("✅ Signals connected")
	print("🔌 Connecting to: ", SERVER_URL)

	# Connect to server
	client.connect_to_server(SERVER_URL)
	status_label.text = "Connecting..."

func _process(delta: float) -> void:
	# Update player count
	player_count_label.text = "Players: %d" % players.size()

	# Send our position if connected (throttled to 1 msg/sec)
	if client and my_client_id != "" and players.has(my_client_id):
		position_send_timer -= delta

		if position_send_timer <= 0.0:
			var player = players[my_client_id]
			var current_pos = player.position

			# Only send if position changed
			if current_pos.distance_to(last_sent_position) > 0.1:
				send_position()
				last_sent_position = current_pos

			# Reset timer
			position_send_timer = POSITION_SEND_INTERVAL

## Signal handlers

func _on_client_connected() -> void:
	print("✅ Connected to server!")
	status_label.text = "Connected!"
	status_label.modulate = Color.GREEN

func _on_client_disconnected(reason: String) -> void:
	print("❌ Disconnected: ", reason)
	status_label.text = "Disconnected: " + reason
	status_label.modulate = Color.RED

	# Clean up players
	for player in players.values():
		player.queue_free()
	players.clear()
	my_client_id = ""

func _on_client_message(message: String) -> void:
	print("📨 Received: ", message)

	var json = JSON.new()
	if json.parse(message) != OK:
		print("⚠️  Failed to parse JSON")
		return

	var data: Dictionary = json.data

	# Handle welcome message
	if data.has("type") and data["type"] == "welcome":
		my_client_id = data["client_id"]
		print("🎉 My client ID: ", my_client_id)

		# Create our own player
		create_player(my_client_id, true)

		# Send initial position immediately
		send_position()
		return

	# Handle player disconnect
	if data.has("type") and data["type"] == "player_left":
		var left_client_id: String = data["client_id"]
		if players.has(left_client_id):
			print("👋 Player ", left_client_id, " left")
			players[left_client_id].queue_free()
			players.erase(left_client_id)
		return

	# Handle player updates
	if data.has("client_id"):
		var client_id: String = data["client_id"]
		var x: float = data.get("x", 400.0)
		var y: float = data.get("y", 300.0)

		# Update or create other players
		if client_id != my_client_id:
			update_player(client_id, Vector2(x, y))

func _on_client_error(error: String) -> void:
	print("❌ Error: ", error)
	status_label.text = "Error: " + error
	status_label.modulate = Color.RED

## Player management

func create_player(client_id: String, is_local: bool) -> void:
	if players.has(client_id):
		return

	var player = PLAYER_SCENE.instantiate()
	player.client_id = client_id
	player.is_local = is_local
	player.position = Vector2(640, 360)  # Center of screen

	add_child(player)
	players[client_id] = player

	print("➕ Created player ", client_id, " (local: ", is_local, ")")

func update_player(client_id: String, pos: Vector2) -> void:
	if not players.has(client_id):
		create_player(client_id, false)

	var player = players[client_id]
	if not player.is_local:
		player.set_network_position(pos)

func send_position() -> void:
	if not players.has(my_client_id):
		return

	var player = players[my_client_id]
	var data = {
		"client_id": my_client_id,
		"x": player.position.x,
		"y": player.position.y
	}

	var json_str = JSON.stringify(data)
	client.send_message(json_str)

## Input handling

func _input(_event: InputEvent) -> void:
	# Only move if we have a local player
	if my_client_id == "" or not players.has(my_client_id):
		return

	var player = players[my_client_id]

	# Move with arrow keys
	var velocity = Vector2.ZERO
	if Input.is_key_pressed(KEY_LEFT) or Input.is_key_pressed(KEY_A):
		velocity.x -= 1
	if Input.is_key_pressed(KEY_RIGHT) or Input.is_key_pressed(KEY_D):
		velocity.x += 1
	if Input.is_key_pressed(KEY_UP) or Input.is_key_pressed(KEY_W):
		velocity.y -= 1
	if Input.is_key_pressed(KEY_DOWN) or Input.is_key_pressed(KEY_S):
		velocity.y += 1

	if velocity.length() > 0:
		velocity = velocity.normalized() * 300 * get_process_delta_time()
		player.position += velocity

		# Clamp to screen
		player.position.x = clamp(player.position.x, 50, 1230)
		player.position.y = clamp(player.position.y, 50, 670)
