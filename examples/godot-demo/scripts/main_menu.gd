extends Control

## Main menu scene for GodotNetLink Demo
##
## Launch client to connect to external Rust server

@onready var client_button := $VBoxContainer/ClientButton
@onready var quit_button := $VBoxContainer/QuitButton

func _ready() -> void:
	client_button.pressed.connect(_on_client_pressed)
	quit_button.pressed.connect(_on_quit_pressed)

func _on_client_pressed() -> void:
	print("Launching client...")
	get_tree().change_scene_to_file("res://scenes/client.tscn")

func _on_quit_pressed() -> void:
	get_tree().quit()
