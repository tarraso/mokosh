extends Node2D

## Main level script that manages players and boxes

@onready var client: Node = $Client
@onready var players_container := $Players
@onready var boxes_container := $Boxes

# Spawned entities
var players := {}  # player_id (String) -> Player node
var boxes := {}    # box_id (int) -> Box node

func _ready() -> void:
	client.connected_to_server.connect(_on_connected)
	client.disconnected_from_server.connect(_on_disconnected)
	client.game_state_received.connect(_on_game_state_received)

	# Auto-connect on start
	client.connect_to_server()

func _on_connected() -> void:
	print("Level: Connected to server")

func _on_disconnected() -> void:
	print("Level: Disconnected from server")

func _on_game_state_received(state: Dictionary) -> void:
	# Update players
	if state.has("players"):
		var player_states = state["players"]
		for player_state in player_states:
			var player_id = player_state["id"]
			var pos = Vector2(player_state["position"]["x"], player_state["position"]["y"])

			if not players.has(player_id):
				# Spawn new player
				var player_node = _spawn_player(player_id, pos)
				players[player_id] = player_node
			else:
				# Update existing player
				players[player_id].position = pos

	# Update boxes
	if state.has("boxes"):
		var box_states = state["boxes"]
		for box_state in box_states:
			var box_id = box_state["id"]
			var pos = Vector2(box_state["position"]["x"], box_state["position"]["y"])

			if not boxes.has(box_id):
				# Spawn new box
				var box_node = _spawn_box(box_id, pos)
				boxes[box_id] = box_node
			else:
				# Update existing box
				boxes[box_id].position = pos

func _spawn_player(player_id: String, pos: Vector2) -> ColorRect:
	var player = ColorRect.new()
	player.custom_minimum_size = Vector2(32, 32)
	player.color = Color.GREEN  # All players green for now
	player.position = pos

	players_container.add_child(player)
	print("Spawned player ", player_id, " at ", pos)
	return player

func _spawn_box(box_id: int, pos: Vector2) -> ColorRect:
	var box = ColorRect.new()
	box.custom_minimum_size = Vector2(32, 32)
	box.color = Color.BROWN
	box.position = pos

	boxes_container.add_child(box)
	print("Spawned box ", box_id, " at ", pos)
	return box

func _process(delta: float) -> void:
	# No interpolation - just direct position updates from server

	# Send input for local player
	var move_x = Input.get_axis("ui_left", "ui_right")
	var jump = Input.is_action_just_pressed("ui_up")

	if abs(move_x) > 0.01 or jump:
		client.send_input(move_x, jump)
