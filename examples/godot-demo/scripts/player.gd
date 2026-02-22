extends CharacterBody2D

## Player representation in multiplayer game

var client_id: String = ""
var is_local := false

# Interpolation for smooth network movement
var target_position: Vector2 = Vector2.ZERO
var interpolation_speed: float = 10.0

func _ready() -> void:
	target_position = position

	# Set color based on whether this is the local player
	if is_local:
		$ColorRect.color = Color.GREEN
		$Label.text = "You"
	else:
		$ColorRect.color = Color.RED
		$Label.text = "Player"

func _process(delta: float) -> void:
	# Interpolate position for remote players
	if not is_local:
		position = position.lerp(target_position, interpolation_speed * delta)

func set_network_position(new_position: Vector2) -> void:
	target_position = new_position
