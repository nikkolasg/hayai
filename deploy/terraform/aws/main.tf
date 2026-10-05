locals {
  # P2P port that peers reach: zakurad on Testnet and Mainnet, hayaid on Regtest.
  p2p_port = { testnet = 18233, mainnet = 8233, regtest = 18344 }[var.network]
  admin_ports = concat(
    [3000, 9090, 9093, 9999, 19101],
    var.network == "regtest" ? [18345] : [],
  )
  subnet_id = coalesce(var.subnet_id, data.aws_subnets.default.ids[0])
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

resource "aws_security_group" "node" {
  name        = "${var.name}-node"
  description = "hayai node: public P2P, admin ports from admin_cidr only"
  vpc_id      = data.aws_subnet.selected.vpc_id
}

resource "aws_vpc_security_group_ingress_rule" "p2p_ipv4" {
  security_group_id = aws_security_group.node.id
  description       = "P2P"
  ip_protocol       = "tcp"
  from_port         = local.p2p_port
  to_port           = local.p2p_port
  cidr_ipv4         = "0.0.0.0/0"
}

resource "aws_vpc_security_group_ingress_rule" "p2p_ipv6" {
  security_group_id = aws_security_group.node.id
  description       = "P2P"
  ip_protocol       = "tcp"
  from_port         = local.p2p_port
  to_port           = local.p2p_port
  cidr_ipv6         = "::/0"
}

resource "aws_vpc_security_group_ingress_rule" "admin" {
  for_each = var.admin_cidr == null ? toset([]) : toset([for p in local.admin_ports : tostring(p)])

  security_group_id = aws_security_group.node.id
  description       = "admin port ${each.value}"
  ip_protocol       = "tcp"
  from_port         = tonumber(each.value)
  to_port           = tonumber(each.value)
  cidr_ipv4         = var.admin_cidr
}

resource "aws_vpc_security_group_egress_rule" "all_ipv4" {
  security_group_id = aws_security_group.node.id
  ip_protocol       = "-1"
  cidr_ipv4         = "0.0.0.0/0"
}

resource "aws_vpc_security_group_egress_rule" "all_ipv6" {
  security_group_id = aws_security_group.node.id
  ip_protocol       = "-1"
  cidr_ipv6         = "::/0"
}

# SSM Session Manager: shell and port forwards without SSH.
resource "aws_iam_role" "node" {
  name = "${var.name}-node"
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
  role       = aws_iam_role.node.name
  policy_arn = "arn:aws:iam::aws:policy/AmazonSSMManagedInstanceCore"
}

resource "aws_iam_instance_profile" "node" {
  name = "${var.name}-node"
  role = aws_iam_role.node.name
}

resource "aws_ebs_volume" "data" {
  availability_zone = data.aws_subnet.selected.availability_zone
  type              = "gp3"
  size              = var.data_volume_gb
  iops              = var.data_volume_iops
  throughput        = var.data_volume_throughput
  encrypted         = true

  tags = { Name = "${var.name}-data" }
}

resource "aws_instance" "node" {
  ami                         = data.aws_ami.ubuntu.id
  instance_type               = var.instance_type
  subnet_id                   = local.subnet_id
  vpc_security_group_ids      = [aws_security_group.node.id]
  iam_instance_profile        = aws_iam_instance_profile.node.name
  key_name                    = var.key_name
  associate_public_ip_address = true
  user_data_replace_on_change = true

  user_data = templatefile("${path.module}/cloud-init.yaml.tftpl", {
    data_volume_id = replace(aws_ebs_volume.data.id, "-", "")
    network        = var.network
    admin_bind     = var.admin_cidr == null ? "127.0.0.1" : "0.0.0.0"
    crypto_backend = var.crypto_backend
    hayai_repo     = var.hayai_repo
    hayai_ref      = var.hayai_ref
    zakura_repo    = var.zakura_repo == null ? "" : var.zakura_repo
    zakura_ref     = var.zakura_ref
  })

  root_block_device {
    volume_type = "gp3"
    volume_size = var.root_volume_gb
    encrypted   = true
  }

  metadata_options {
    http_tokens = "required"
  }

  lifecycle {
    precondition {
      condition     = var.network == "regtest" || var.zakura_repo != null
      error_message = "network = \"testnet\" and \"mainnet\" need zakura_repo: hayaid in shadow mode follows a zakurad."
    }
  }
}

resource "aws_volume_attachment" "data" {
  device_name = "/dev/sdf"
  volume_id   = aws_ebs_volume.data.id
  instance_id = aws_instance.node.id
}
