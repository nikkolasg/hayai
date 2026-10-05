variable "region" {
  description = "AWS region of every resource."
  type        = string
}

variable "name" {
  description = "Name of the instance and prefix of every resource name."
  type        = string
  default     = "hayai"
}

variable "network" {
  description = "Compose profile: testnet or mainnet (zakurad and hayaid in shadow mode) or regtest (hayaid in full mode). The operator chooses mainnet; docs/hayaid.md (Mainnet) lists the consensus rules that hayaid does not enforce yet."
  type        = string
  default     = "testnet"

  validation {
    condition     = contains(["testnet", "mainnet", "regtest"], var.network)
    error_message = "network must be testnet, mainnet or regtest."
  }
}

variable "instance_type" {
  description = "EC2 instance type. 16 GiB of memory holds the mainnet coin set of the memory backend (about 3.8 GB, docs/architecture.md) beside zakurad."
  type        = string
  default     = "m7i.xlarge"
}

variable "root_volume_gb" {
  description = "Size of the root volume (operating system, source checkouts) in GiB."
  type        = number
  default     = 30
}

variable "data_volume_gb" {
  description = "Size of the gp3 data volume in GiB. It holds /var/lib/docker: the images, the hayaid and zakurad volumes, Prometheus."
  type        = number
  default     = 300
}

variable "data_volume_iops" {
  description = "Provisioned IOPS of the gp3 data volume (3000 to 16000)."
  type        = number
  default     = 3000
}

variable "data_volume_throughput" {
  description = "Provisioned throughput of the gp3 data volume in MiB/s (125 to 1000)."
  type        = number
  default     = 125
}

variable "admin_cidr" {
  description = "CIDR that reaches the admin ports (Grafana 3000, Prometheus 9090, Alertmanager 9093, metrics 9999 and 19101, Regtest RPC 18345 with cookie authentication and no TLS). null: no admin port is open, use an SSM port forward."
  type        = string
  default     = null

  validation {
    condition     = var.admin_cidr == null || can(cidrhost(var.admin_cidr, 0))
    error_message = "admin_cidr must be a CIDR block, for example 198.51.100.7/32."
  }
}

variable "key_name" {
  description = "Optional EC2 key pair for SSH. null: no key; SSM Session Manager gives the shell. Port 22 is never opened by this module."
  type        = string
  default     = null
}

variable "subnet_id" {
  description = "Subnet of the instance (with a route to the internet). null: the first default subnet of the default VPC."
  type        = string
  default     = null
}

variable "hayai_repo" {
  description = "Git URL of the hayai repository that the instance clones and builds."
  type        = string
}

variable "hayai_ref" {
  description = "Branch, tag or commit of hayai to build."
  type        = string
  default     = "main"
}

variable "crypto_backend" {
  description = "Crypto backend of the hayaid build: upstream or zakura."
  type        = string
  default     = "upstream"

  validation {
    condition     = contains(["upstream", "zakura"], var.crypto_backend)
    error_message = "crypto_backend must be upstream or zakura."
  }
}

variable "zakura_repo" {
  description = "Git URL of the Zakura repository, built into the zakurad image of the testnet and mainnet profiles. Required when network is testnet or mainnet."
  type        = string
  default     = null
}

variable "zakura_ref" {
  description = "Branch, tag or commit of Zakura to build."
  type        = string
  default     = "main"
}

variable "tags" {
  description = "Extra tags of every resource."
  type        = map(string)
  default     = {}
}
