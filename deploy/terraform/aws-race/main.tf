locals {
  subnet_id = coalesce(var.subnet_id, data.aws_subnets.default.ids[0])

  # The two node machines. They differ in the role only: the instance type, the volumes
  # and the subnet (and so the availability zone) are the same.
  nodes = {
    zakurad = {
      metrics_port = 9999
      image        = "zakurad:race"
    }
    hayaid = {
      metrics_port = 19101
      image        = "hayaid:race"
    }
  }

  # Ports of each node machine that only machine C reaches: the metrics of the node and
  # node-exporter.
  metrics_rules = merge([
    for node, settings in local.nodes : {
      "${node}-metrics"  = { node = node, port = settings.metrics_port }
      "${node}-exporter" = { node = node, port = 9100 }
    }
  ]...)

  start_time = formatdate("YYYY-MM-DD hh:mm:ss", var.start_at)

  # The values that follow the network.
  network_settings = {
    mainnet = { label = "Mainnet", p2p_port = 8233, data_volume_gb = 400, hayai_backend = "upstream" }
    testnet = { label = "Testnet", p2p_port = 18233, data_volume_gb = 200, hayai_backend = "zakura" }
  }
  network        = local.network_settings[var.network]
  data_volume_gb = coalesce(var.data_volume_gb, local.network.data_volume_gb)
}

data "aws_ami" "ubuntu" {
  most_recent = true
  owners      = ["099720109477"] # Canonical

  filter {
    name   = "name"
    values = ["ubuntu/images/hvm-ssd-gp3/ubuntu-noble-24.04-amd64-server-*"]
  }

  filter {
    name   = "architecture"
    values = ["x86_64"]
  }
}

data "aws_vpc" "default" {
  default = true
}

data "aws_subnets" "default" {
  filter {
    name   = "vpc-id"
    values = [data.aws_vpc.default.id]
  }

  filter {
    name   = "default-for-az"
    values = ["true"]
  }
}

data "aws_subnet" "selected" {
  id = local.subnet_id
}

# Security groups. No rule opens an RPC port: each RPC server listens on the loopback
# address of its machine.
resource "aws_security_group" "node" {
  for_each = local.nodes

  name        = "${var.name}-${each.key}"
  description = "sync race, ${each.key}: public P2P, metrics from the monitoring machine, SSH from admin_cidr"
  vpc_id      = data.aws_subnet.selected.vpc_id

  tags = { Name = "${var.name}-${each.key}" }
}

resource "aws_security_group" "monitor" {
  name        = "${var.name}-monitor"
  description = "sync race, monitoring: Grafana and SSH from admin_cidr"
  vpc_id      = data.aws_subnet.selected.vpc_id

  tags = { Name = "${var.name}-monitor" }
}

# P2P of both nodes (TCP 8233 on Mainnet, 18233 on Testnet). zakurad runs its legacy
# stack only (docker/race/config), which has no UDP port.
resource "aws_vpc_security_group_ingress_rule" "p2p_ipv4" {
  for_each = local.nodes

  security_group_id = aws_security_group.node[each.key].id
  description       = "${local.network.label} P2P"
  ip_protocol       = "tcp"
  from_port         = local.network.p2p_port
  to_port           = local.network.p2p_port
  cidr_ipv4         = "0.0.0.0/0"
}

resource "aws_vpc_security_group_ingress_rule" "p2p_ipv6" {
  for_each = local.nodes

  security_group_id = aws_security_group.node[each.key].id
  description       = "${local.network.label} P2P"
  ip_protocol       = "tcp"
  from_port         = local.network.p2p_port
  to_port           = local.network.p2p_port
  cidr_ipv6         = "::/0"
}

resource "aws_vpc_security_group_ingress_rule" "metrics" {
  for_each = local.metrics_rules

  security_group_id            = aws_security_group.node[each.value.node].id
  description                  = "${each.key} from the monitoring machine"
  ip_protocol                  = "tcp"
  from_port                    = each.value.port
  to_port                      = each.value.port
  referenced_security_group_id = aws_security_group.monitor.id
}

resource "aws_vpc_security_group_ingress_rule" "node_ssh" {
  for_each = local.nodes

  security_group_id = aws_security_group.node[each.key].id
  description       = "SSH from admin_cidr"
  ip_protocol       = "tcp"
  from_port         = 22
  to_port           = 22
  cidr_ipv4         = var.admin_cidr
}

resource "aws_vpc_security_group_ingress_rule" "monitor_admin" {
  for_each = toset(["22", "3000"])

  security_group_id = aws_security_group.monitor.id
  description       = "port ${each.value} from admin_cidr"
  ip_protocol       = "tcp"
  from_port         = tonumber(each.value)
  to_port           = tonumber(each.value)
  cidr_ipv4         = var.admin_cidr
}

resource "aws_vpc_security_group_egress_rule" "node_ipv4" {
  for_each = local.nodes

  security_group_id = aws_security_group.node[each.key].id
  ip_protocol       = "-1"
  cidr_ipv4         = "0.0.0.0/0"
}

resource "aws_vpc_security_group_egress_rule" "node_ipv6" {
  for_each = local.nodes

  security_group_id = aws_security_group.node[each.key].id
  ip_protocol       = "-1"
  cidr_ipv6         = "::/0"
}

resource "aws_vpc_security_group_egress_rule" "monitor_ipv4" {
  security_group_id = aws_security_group.monitor.id
  ip_protocol       = "-1"
  cidr_ipv4         = "0.0.0.0/0"
}

# SSM Session Manager: a shell without SSH.
resource "aws_iam_role" "race" {
  name = var.name
  assume_role_policy = jsonencode({
    Version = "2012-10-17"
    Statement = [{
      Effect    = "Allow"
      Principal = { Service = "ec2.amazonaws.com" }
      Action    = "sts:AssumeRole"
    }]
  })
}

resource "aws_iam_role_policy_attachment" "ssm" {
  role       = aws_iam_role.race.name
  policy_arn = "arn:aws:iam::aws:policy/AmazonSSMManagedInstanceCore"
}

resource "aws_iam_instance_profile" "race" {
  name = var.name
  role = aws_iam_role.race.name
}

resource "aws_ebs_volume" "data" {
  for_each = local.nodes

  availability_zone = data.aws_subnet.selected.availability_zone
  type              = "gp3"
  size              = local.data_volume_gb
  iops              = var.data_volume_iops
  throughput        = var.data_volume_throughput
  encrypted         = true

  tags = { Name = "${var.name}-${each.key}-data" }
}

resource "aws_instance" "node" {
  for_each = local.nodes

  ami                         = data.aws_ami.ubuntu.id
  instance_type               = var.node_instance_type
  subnet_id                   = local.subnet_id
  vpc_security_group_ids      = [aws_security_group.node[each.key].id]
  iam_instance_profile        = aws_iam_instance_profile.race.name
  key_name                    = var.key_name
  associate_public_ip_address = true
  user_data_replace_on_change = true

  user_data = templatefile("${path.module}/cloud-init.node.yaml.tftpl", {
    role                 = each.key
    image                = each.value.image
    data_volume_id       = replace(aws_ebs_volume.data[each.key].id, "-", "")
    hayai_repo           = var.hayai_repo
    hayai_ref            = var.hayai_ref
    zakura_repo          = var.zakura_repo
    zakura_ref           = var.zakura_ref
    start_at             = local.start_time
    node_cpus            = var.node_cpus
    node_memory          = var.node_memory
    rpc_caller           = var.rpc_caller
    rpc_caller_interval  = var.rpc_caller_interval
    rpc_caller_long_poll = var.rpc_caller_long_poll
    network              = var.network
    hayai_backend        = local.network.hayai_backend
  })

  root_block_device {
    volume_type = "gp3"
    volume_size = var.root_volume_gb
    encrypted   = true
  }

  metadata_options {
    http_tokens = "required"
  }

  tags = { Name = "${var.name}-${each.key}" }
}

resource "aws_volume_attachment" "data" {
  for_each = local.nodes

  device_name = "/dev/sdf"
  volume_id   = aws_ebs_volume.data[each.key].id
  instance_id = aws_instance.node[each.key].id
}

resource "aws_instance" "monitor" {
  ami                         = data.aws_ami.ubuntu.id
  instance_type               = var.monitor_instance_type
  subnet_id                   = local.subnet_id
  vpc_security_group_ids      = [aws_security_group.monitor.id]
  iam_instance_profile        = aws_iam_instance_profile.race.name
  key_name                    = var.key_name
  associate_public_ip_address = true
  user_data_replace_on_change = true

  # Prometheus reaches the node machines on their private addresses.
  user_data = templatefile("${path.module}/cloud-init.monitor.yaml.tftpl", {
    hayai_repo   = var.hayai_repo
    hayai_ref    = var.hayai_ref
    zakurad_host = aws_instance.node["zakurad"].private_ip
    hayaid_host  = aws_instance.node["hayaid"].private_ip
    network      = var.network
  })

  root_block_device {
    volume_type = "gp3"
    volume_size = var.root_volume_gb
    encrypted   = true
  }

  metadata_options {
    http_tokens = "required"
  }

  tags = { Name = "${var.name}-monitor" }
}
