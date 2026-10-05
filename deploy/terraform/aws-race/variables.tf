variable "region" {
  description = "AWS region of every resource."
  type        = string
}

variable "name" {
  description = "Prefix of every resource name."
  type        = string
  default     = "hayai-race"
}

variable "network" {
  description = "Zcash network of both nodes: mainnet or testnet. It selects the node configurations (docker/race/config), the P2P port (8233 or 18233), the crypto backend of the hayaid image (mainnet: the default build; testnet: zakura) and the default size of the data volume."
  type        = string
  default     = "mainnet"

  validation {
    condition     = contains(["mainnet", "testnet"], var.network)
    error_message = "network must be mainnet or testnet."
  }
}

variable "subnet_id" {
  description = "Subnet of the three instances (with a route to the internet). It gives the availability zone, which is the same for the two node machines. null: the first default subnet of the default VPC."
  type        = string
  default     = null
}

variable "node_instance_type" {
  description = "EC2 instance type of machine A (zakurad) and of machine B (hayaid). The two machines always have the same type. The default has 8 vCPUs and 32 GiB: the recommended CPU and memory of Zakura (docs/sync-race.md, Machine size)."
  type        = string
  default     = "m7i.2xlarge"
}

variable "monitor_instance_type" {
  description = "EC2 instance type of machine C (Prometheus and Grafana)."
  type        = string
  default     = "t3.small"
}

variable "root_volume_gb" {
  description = "Size of the root volume of each instance in GiB (operating system, source checkouts)."
  type        = number
  default     = 30
}

variable "data_volume_gb" {
  description = "Size of the gp3 data volume of each node machine in GiB. It holds /var/lib/docker: the image build and the node data. The two machines always have the same size. null: 400 for mainnet and 200 for testnet. docs/sync-race.md (Machine size) gives the source of these values."
  type        = number
  default     = null
}

variable "data_volume_iops" {
  description = "Provisioned IOPS of each gp3 data volume (3000 to 16000)."
  type        = number
  default     = 3000
}

variable "data_volume_throughput" {
  description = "Provisioned throughput of each gp3 data volume in MiB/s (125 to 1000)."
  type        = number
  default     = 125
}

variable "admin_cidr" {
  description = "CIDR that reaches SSH (22) on the three machines and Grafana (3000) on machine C, for example 198.51.100.7/32."
  type        = string

  validation {
    condition     = can(cidrhost(var.admin_cidr, 0))
    error_message = "admin_cidr must be a CIDR block, for example 198.51.100.7/32."
  }
}

variable "key_name" {
  description = "EC2 key pair for SSH from admin_cidr. null: no key; SSM Session Manager gives the shell."
  type        = string
  default     = null
}

variable "start_at" {
  description = "UTC time at which both nodes start, as RFC 3339 with the minute, for example 2026-11-02T14:00:00Z. Each node machine waits for this time on its own clock (chrony). Choose a time 90 minutes or more after `terraform apply`: each machine builds its image first. A machine that is not ready at this time starts its node late and records the delay in /var/log/race-start.log."
  type        = string

  validation {
    condition     = can(formatdate("YYYY-MM-DD'T'hh:mm:ssZ", var.start_at))
    error_message = "start_at must be an RFC 3339 time, for example 2026-11-02T14:00:00Z."
  }
}

variable "hayai_repo" {
  description = "Git URL of the hayai repository. Each machine clones it: it has the race files (docker/race), and machine B builds hayaid from it."
  type        = string
}

variable "hayai_ref" {
  description = "Commit of hayai for the race files and for the hayaid image. Use a full commit hash: a branch can move between the three clones."
  type        = string
}

variable "zakura_repo" {
  description = "Git URL of the Zakura repository. Machine A builds zakurad from it."
  type        = string
}

variable "zakura_ref" {
  description = "Commit of Zakura for the zakurad image. The default is the commit of docs/sync-race.md (Zakura 1.6.0)."
  type        = string
  default     = "13779158253cfe315f73eadffb9b4c93c25e82a5"
}

variable "node_cpus" {
  description = "CPU limit of both node containers. 0: no limit."
  type        = number
  default     = 0
}

variable "node_memory" {
  description = "Memory limit of both node containers, as a compose value (for example 24g). 0: no limit."
  type        = string
  default     = "0"
}

variable "rpc_caller" {
  description = "true: an RPC caller (scripts/race_rpc_caller.py) runs beside each node and calls getblocktemplate. false: no RPC caller."
  type        = bool
  default     = true
}

variable "rpc_caller_interval" {
  description = "Seconds between two getblocktemplate calls of each RPC caller."
  type        = number
  default     = 5
}

variable "rpc_caller_long_poll" {
  description = "true: each RPC caller also holds one getblocktemplate long poll. The mean of rpc_request_duration_seconds then contains the wait of the long poll."
  type        = bool
  default     = false
}

variable "tags" {
  description = "Extra tags of every resource."
  type        = map(string)
  default     = {}
}
