output "grafana_url" {
  description = "Grafana on machine C (user admin). docs/sync-race.md describes the dashboards."
  value       = "http://${aws_instance.monitor.public_ip}:3000"
}

output "grafana_password_command" {
  description = "Run in a shell on machine C: prints the Grafana admin password that the first boot generated."
  value       = "sudo cat /opt/race/hayai/docker/race/secrets/grafana_admin_password"
}

output "network" {
  description = "Zcash network of both nodes."
  value       = var.network
}

output "start_at" {
  description = "UTC time at which both nodes start."
  value       = var.start_at
}

output "public_ips" {
  description = "Public IPv4 addresses of machine A (zakurad), machine B (hayaid) and machine C (monitor)."
  value = {
    zakurad = aws_instance.node["zakurad"].public_ip
    hayaid  = aws_instance.node["hayaid"].public_ip
    monitor = aws_instance.monitor.public_ip
  }
}

output "ssm_shell_commands" {
  description = "Shell on each machine through SSM Session Manager."
  value = {
    zakurad = "aws ssm start-session --region ${var.region} --target ${aws_instance.node["zakurad"].id}"
    hayaid  = "aws ssm start-session --region ${var.region} --target ${aws_instance.node["hayaid"].id}"
    monitor = "aws ssm start-session --region ${var.region} --target ${aws_instance.monitor.id}"
  }
}

output "start_log_command" {
  description = "Run in a shell on machine A and on machine B: the bootstrap log, and the planned and the actual start time of the node."
  value       = "sudo journalctl -u race-bootstrap -u race-start --no-pager | tail -n 50; sudo cat /var/log/race-start.log"
}
