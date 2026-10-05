output "instance_id" {
  description = "EC2 instance id (the SSM target)."
  value       = aws_instance.node.id
}

output "public_ip" {
  description = "Public IPv4 address. It changes when the instance stops and starts."
  value       = aws_instance.node.public_ip
}

output "p2p_endpoint" {
  description = "P2P address that other nodes dial: zakurad on Testnet and Mainnet, hayaid on Regtest."
  value       = "${aws_instance.node.public_ip}:${local.p2p_port}"
}

output "ssm_shell_command" {
  description = "Shell on the instance through SSM Session Manager."
  value       = "aws ssm start-session --region ${var.region} --target ${aws_instance.node.id}"
}

output "grafana_port_forward_command" {
  description = "Forwards local port 3000 to Grafana. Then open grafana_url."
  value       = "aws ssm start-session --region ${var.region} --target ${aws_instance.node.id} --document-name AWS-StartPortForwardingSession --parameters '{\"portNumber\":[\"3000\"],\"localPortNumber\":[\"3000\"]}'"
}

output "grafana_url" {
  description = "Grafana through the SSM port forward (user admin)."
  value       = "http://localhost:3000"
}

output "grafana_password_command" {
  description = "Run in an SSM shell: prints the Grafana admin password that cloud-init generated."
  value       = "sudo cat /opt/hayai/src/docker/secrets/grafana_admin_password"
}

output "bootstrap_log_command" {
  description = "Run in an SSM shell: follows the bootstrap (clone, image builds, compose start)."
  value       = "sudo journalctl -u hayai-bootstrap -u hayai-stack -f"
}
